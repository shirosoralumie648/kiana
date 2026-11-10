//! `7443-model-command-cached-match` / `7027-credential-refresh-hang`（InteractiveMode 侧）与
//! Anthropic 订阅警告、登录/登出选项的移植。

#![allow(
    clippy::await_holding_lock,
    reason = "主题初始化是进程级状态：同文件的用例必须串行（TS 的 beforeAll(initTheme) 等价）"
)]

mod common;

use std::sync::{Mutex, MutexGuard};

use kiana_ai::types::Model;
use kiana_config::settings::TuiMode;
use kiana_config::settings::schema::WarningSettings;
use kiana_interactive::interactive_mode::{InteractiveMode, InteractiveModeOptions};

static THEME_LOCK: Mutex<()> = Mutex::new(());

fn theme() -> MutexGuard<'static, ()> {
    let guard = THEME_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    kiana_theme::init_theme(Some("dark"), false);
    guard
}

struct Fixture {
    mode: InteractiveMode,
    _faux: kiana_ai::providers::faux::FauxCore,
    _temp: tempfile::TempDir,
    _engine: common::TestEngine,
    _cwd: String,
    agent_dir: String,
}

async fn fixture() -> Fixture {
    fixture_with_offline(true).await
}

/// `offline=false` 时 `ModelRuntime` 允许联网刷新（issue #7027 的用例需要真实调用
/// provider 的 `fetchModels`）。
async fn fixture_with_offline(offline: bool) -> Fixture {
    unsafe {
        if offline {
            std::env::set_var("KIANA_OFFLINE", "1");
        } else {
            std::env::remove_var("KIANA_OFFLINE");
        }
    }
    let host = common::runtime_host(common::RuntimeHostOptions::new()).await;
    unsafe { std::env::set_var("KIANA_OFFLINE", "1") };
    let common::RuntimeHost {
        runtime,
        faux,
        temp_dir,
        cwd,
        engine,
        agent_dir,
        ..
    } = host;
    let mode = InteractiveMode::new(
        runtime,
        InteractiveModeOptions {
            terminal: Some(Box::new(common::TestTerminal::new())),
            tui_mode: Some(TuiMode::Regular),
            ..Default::default()
        },
    );
    Fixture {
        mode,
        _faux: faux,
        _temp: temp_dir,
        _engine: engine,
        _cwd: cwd,
        agent_dir,
    }
}

fn model(provider: &str, id: &str) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: kiana_ai::types::Api::from("faux"),
        provider: provider.to_string(),
        base_url: "https://example.test/v1".to_string(),
        input: vec![],
        input_limits: None,
        cost: Default::default(),
        headers: None,
        model_type: None,
        reasoning: Some(false),
        thinking_level_map: None,
        prompt_cache: None,
        context_window: None,
        max_tokens: None,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
        output: None,
    }
}

// ---------------------------------------------------------------------------
// issue #7443：/model 缓存命中不触发刷新
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn matches_the_availability_snapshot_without_starting_a_catalog_refresh() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let cached_model = fixture.mode.session().model().expect("bootstrap model");
        let found = fixture
            .mode
            .test_find_exact_model_match(&cached_model.id)
            .await;
        assert_eq!(found.map(|model| model.id), Some(cached_model.id));
        // 命中缓存时不显示 "Refreshing model catalogs…"。
        assert_eq!(fixture.mode.test_last_status_message(), "");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn uses_a_caller_owned_deadline_only_after_a_cache_miss() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let found = fixture.mode.test_find_exact_model_match("not-cached").await;
        assert!(found.is_none());
        // 缓存未命中时先显示刷新状态（刷新本身在离线模式下立即结束）。
        assert_eq!(
            fixture.mode.test_last_status_message(),
            "Refreshing model catalogs…"
        );
    })
    .await;
}

/// review：`showModelsSelector` 的 `dispose` 必须置 `disposed = true`。
///
/// TS 在刷新完成后先检查 `if (disposed) return;`：用户在选择器关闭后才完成的刷新
/// 不得再改会话的 scoped models / 触发重绘。这里先同步挂载选择器（刷新任务已排队但
/// 尚未运行），再用 `/settings` disposen 它，最后让刷新跑完。
#[tokio::test(flavor = "current_thread")]
async fn closing_scoped_models_before_the_refresh_finishes_keeps_the_session_unchanged() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let available = fixture
            .mode
            .session()
            .model_runtime()
            .get_available_snapshot();
        assert!(
            available.len() >= 2,
            "fixture needs at least two models: {available:?}"
        );
        // 配置 enabledModels 但要保留一个未选中的模型：这样刷新续体会走
        // `updateSessionModels` 的 setScopedModels(非空) 分支。
        let selected = format!("{}/{}", available[0].provider, available[0].id);
        fixture
            .mode
            .settings_manager()
            .set_enabled_models(Some(vec![selected]));

        fixture.mode.test_show_models_selector();
        assert!(fixture.mode.test_has_active_selector());
        // 同步 dispose（挂载 /settings 会先 dispose 上一个选择器）。
        fixture.mode.test_dispatch_submit("/settings");
        common::yield_tasks(100).await;

        assert!(
            fixture.mode.test_scoped_model_keys().is_empty(),
            "refresh continuation must not mutate scoped models after dispose: {:?}",
            fixture.mode.test_scoped_model_keys()
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// Anthropic 订阅警告（interactive-mode-anthropic-warning.test.ts）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn does_not_warn_for_non_anthropic_models() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let model = model("openai", "gpt-test");
        fixture
            .mode
            .test_maybe_warn_about_anthropic_subscription_auth(Some(model))
            .await;
        assert!(!fixture.mode.test_anthropic_warning_shown());
        assert!(
            !fixture
                .mode
                .chat_render_text()
                .contains("Anthropic subscription auth is active")
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_warn_when_the_extra_usage_warning_is_disabled() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture
            .mode
            .settings_manager()
            .set_warnings(WarningSettings {
                anthropic_extra_usage: Some(false),
                ..Default::default()
            });
        fixture
            .mode
            .test_maybe_warn_about_anthropic_subscription_auth(Some(model(
                "anthropic",
                "claude-test",
            )))
            .await;
        assert!(!fixture.mode.test_anthropic_warning_shown());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_warn_without_anthropic_credentials() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture
            .mode
            .test_maybe_warn_about_anthropic_subscription_auth(Some(model(
                "anthropic",
                "claude-test",
            )))
            .await;
        assert!(!fixture.mode.test_anthropic_warning_shown());
    })
    .await;
}

/// 保存/恢复会参与 Anthropic 凭证解析的三个环境变量。
struct AnthropicEnv {
    auth_token: Option<std::ffi::OsString>,
    oauth_token: Option<std::ffi::OsString>,
    api_key: Option<std::ffi::OsString>,
}

impl AnthropicEnv {
    fn capture() -> Self {
        AnthropicEnv {
            auth_token: std::env::var_os("ANTHROPIC_AUTH_TOKEN"),
            oauth_token: std::env::var_os("ANTHROPIC_OAUTH_TOKEN"),
            api_key: std::env::var_os("ANTHROPIC_API_KEY"),
        }
    }

    fn clear() {
        unsafe {
            std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
            std::env::remove_var("ANTHROPIC_OAUTH_TOKEN");
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
    }
}

impl Drop for AnthropicEnv {
    fn drop(&mut self) {
        unsafe {
            match &self.auth_token {
                Some(value) => std::env::set_var("ANTHROPIC_AUTH_TOKEN", value),
                None => std::env::remove_var("ANTHROPIC_AUTH_TOKEN"),
            }
            match &self.oauth_token {
                Some(value) => std::env::set_var("ANTHROPIC_OAUTH_TOKEN", value),
                None => std::env::remove_var("ANTHROPIC_OAUTH_TOKEN"),
            }
            match &self.api_key {
                Some(value) => std::env::set_var("ANTHROPIC_API_KEY", value),
                None => std::env::remove_var("ANTHROPIC_API_KEY"),
            }
        }
    }
}

/// TS 只读 `(await getAuth(provider))?.auth.apiKey`；`ANTHROPIC_AUTH_TOKEN` 只解析成
/// `Authorization: Bearer …` 头（apiKey 为 undefined），因此不告警。
#[tokio::test(flavor = "current_thread")]
async fn does_not_warn_for_anthropic_auth_token_headers() {
    let _theme = theme();
    let _env = AnthropicEnv::capture();
    AnthropicEnv::clear();
    unsafe { std::env::set_var("ANTHROPIC_AUTH_TOKEN", "sk-ant-oat01-test") };
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture
            .mode
            .test_maybe_warn_about_anthropic_subscription_auth(Some(model(
                "anthropic",
                "claude-test",
            )))
            .await;
        assert!(!fixture.mode.test_anthropic_warning_shown());
        let rendered = fixture.mode.chat_render_text();
        assert!(!rendered.contains("Anthropic subscription auth is active"));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn warns_once_when_an_anthropic_subscription_api_key_is_used() {
    let _theme = theme();
    let _env = AnthropicEnv::capture();
    AnthropicEnv::clear();
    unsafe { std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-oat01-test") };
    common::run_local(async {
        let mut fixture = fixture().await;
        let anthropic = model("anthropic", "claude-test");
        fixture
            .mode
            .test_maybe_warn_about_anthropic_subscription_auth(Some(anthropic.clone()))
            .await;
        assert!(fixture.mode.test_anthropic_warning_shown());
        let rendered = fixture.mode.chat_render_text();
        assert_eq!(
            rendered
                .matches("Anthropic subscription auth is active")
                .count(),
            1,
            "{rendered}"
        );
        // 第二次调用不再重复警告。
        fixture
            .mode
            .test_maybe_warn_about_anthropic_subscription_auth(Some(anthropic))
            .await;
        assert_eq!(
            fixture
                .mode
                .chat_render_text()
                .matches("Anthropic subscription auth is active")
                .count(),
            1
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// 登录 / 登出选项
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn login_provider_options_list_builtin_providers_sorted_by_name() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        let options = fixture.mode.test_get_login_provider_options(None);
        assert!(options.iter().any(|option| option.id == "anthropic"
            && option.auth_type == kiana_ai::auth::types::AuthType::OAuth));
        assert!(options.iter().any(|option| option.id == "anthropic"
            && option.auth_type == kiana_ai::auth::types::AuthType::ApiKey));
        let names: Vec<String> = options.iter().map(|option| option.name.clone()).collect();
        let mut sorted = names.clone();
        sorted.sort_by(|a, b| kiana_tui::autocomplete::locale_compare(a, b));
        assert_eq!(names, sorted, "providers must be localeCompare-sorted");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn logout_without_stored_credentials_reports_the_exact_message() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_show_oauth_selector("logout").await;
        assert_eq!(
            fixture.mode.test_last_status_message(),
            "No stored credentials to remove. /logout only removes credentials saved by /login; environment variables and models.json config are unchanged."
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn login_command_mounts_the_auth_type_selector() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_handle_login_command(None).await;
        assert!(fixture.mode.test_has_active_selector());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn login_command_with_unknown_provider_opens_the_provider_selector() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture
            .mode
            .test_handle_login_command(Some("does-not-exist".to_string()))
            .await;
        assert!(fixture.mode.test_has_active_selector());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn login_command_with_a_shared_provider_opens_the_auth_type_selector() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture
            .mode
            .test_handle_login_command(Some("anthropic".to_string()))
            .await;
        assert!(fixture.mode.test_has_active_selector());
        assert_eq!(fixture.mode.test_last_status_message(), "");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn login_command_with_an_ambient_provider_opens_the_ambient_dialog() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture
            .mode
            .test_show_ambient_auth_dialog("env-provider", "Env Provider");
        // 环境变量认证：把 LoginDialogComponent 挂到编辑器槽位。
        assert!(fixture.mode.test_selector_child_address().is_some());
        let rendered = fixture.mode.test_selector_render_text();
        assert!(
            rendered.contains("Authentication is configured outside"),
            "{rendered}"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// `/trust`
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn trust_selector_mounts() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_show_trust_selector();
        assert!(fixture.mode.test_has_active_selector());
        assert!(fixture.mode.test_selector_child_address().is_some());
    })
    .await;
}

// ---------------------------------------------------------------------------
// review 回归：Anthropic OAuth 已存储时也必须警告（TS 的
// 「warns when Anthropic OAuth is stored even if token refresh lookup would fail」）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn warns_when_anthropic_oauth_is_stored() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let auth_path = std::path::Path::new(&fixture.agent_dir).join("auth.json");
        std::fs::write(
            &auth_path,
            serde_json::json!({
                "anthropic": {
                    "type": "oauth",
                    "access": "access-token",
                    "refresh": "refresh-token",
                    // 远未来：checkAuth 不需要刷新 token（离线环境）。
                    "expires": 4_102_444_800_000i64
                }
            })
            .to_string(),
        )
        .expect("write auth.json");

        fixture
            .mode
            .test_maybe_warn_about_anthropic_subscription_auth(Some(model(
                "anthropic",
                "claude-test",
            )))
            .await;

        assert!(fixture.mode.test_anthropic_warning_shown());
        let rendered = fixture.mode.chat_render_text();
        assert_eq!(
            rendered
                .matches("Anthropic subscription auth is active")
                .count(),
            1,
            "{rendered}"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// issue #7027：交互式登录必须在有界后台刷新完成前返回
// ---------------------------------------------------------------------------

/// 网络目录刷新一直挂起、只在取消后返回的测试 provider。
fn stalled_login_provider(
    started: std::sync::Arc<std::sync::atomic::AtomicBool>,
    model: Model,
    faux: kiana_ai::providers::faux::FauxCore,
) -> std::sync::Arc<dyn kiana_ai::models::provider::Provider> {
    let fetch: kiana_ai::models::provider::FetchModelsFn =
        std::sync::Arc::new(move |context: kiana_ai::models::RefreshModelsContext| {
            let started = std::sync::Arc::clone(&started);
            Box::pin(async move {
                started.store(true, std::sync::atomic::Ordering::SeqCst);
                // 模拟 TS 测试里 `await new Promise(() => {})` + abort listener：
                // 只在 refresh 的 signal 中止后 resolve。
                context.signal.cancelled().await;
                Ok(Vec::new())
            })
        });
    kiana_ai::models::provider::create_provider(kiana_ai::models::provider::CreateProviderOptions {
        id: "stalled-login".to_string(),
        name: Some("Stalled Login".to_string()),
        base_url: Some("https://example.test/v1".to_string()),
        headers: None,
        auth: kiana_ai::auth::types::ProviderAuth {
            api_key: Some(kiana_ai::auth::helpers::env_api_key_auth(
                "Stalled Login",
                &["KIANA_TEST_STALLED_LOGIN_KEY"],
            )),
            oauth: None,
        },
        models: vec![kiana_ai::types::AnyModel::Chat(model)],
        fetch_models: Some(fetch),
        filter_models: None,
        filter_all_models: None,
        api: Some(kiana_ai::models::provider::ProviderApiImpl::Single(
            std::sync::Arc::new(faux),
        )),
        images: None,
        classifiers: None,
    })
    .expect("stalled login provider")
}

#[tokio::test(flavor = "current_thread")]
async fn interactive_login_completes_before_its_bounded_background_refresh() {
    let _theme = theme();
    unsafe { std::env::set_var("KIANA_TEST_STALLED_LOGIN_KEY", "secret") };
    common::run_local(async {
        let mut fixture = fixture_with_offline(false).await;
        let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let runtime = fixture.mode.session().model_runtime().clone();
        let provider = stalled_login_provider(
            std::sync::Arc::clone(&started),
            model("stalled-login", "dynamic"),
            fixture._faux.clone(),
        );
        runtime
            .register_native_provider(provider)
            .expect("register stalled-login provider");
        // `registerNativeProvider` 会 spawn 一次后台目录刷新；同一 provider 的新刷新会
        // 取代（并中止）旧刷新。先等这次离线刷新结束，避免它把下面的登录后刷新顶掉。
        let _ = runtime
            .refresh(kiana_ai::models::ModelsRefreshOptions {
                allow_network: Some(false),
                providers: Some(vec!["stalled-login".to_string()]),
                ..Default::default()
            })
            .await;

        let previous = fixture.mode.session().model().expect("bootstrap model");
        let began = std::time::Instant::now();
        fixture
            .mode
            .test_complete_provider_authentication(
                "stalled-login",
                "Stalled Login",
                kiana_ai::auth::types::AuthType::ApiKey,
                Some(previous),
            )
            .await;
        assert!(
            began.elapsed() < std::time::Duration::from_secs(5),
            "login must not await the stalled catalog refresh"
        );
        assert!(
            !fixture.mode.chat_render_text().contains("timed out"),
            "no timeout warning before the 15s deadline"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !started.load(std::sync::atomic::Ordering::SeqCst) {
            if std::time::Instant::now() > deadline {
                panic!("refresh never called fetchModels");
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            !fixture
                .mode
                .chat_render_text()
                .contains("its model catalog refresh timed out")
        );

        // 刷新任务已经在 fetch 上挂起；现在冻结时钟并推进 15 秒触发超时分支。
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(15)).await;
        common::wait_until(|| {
            fixture
                .mode
                .chat_render_text()
                .contains("its model catalog refresh timed out")
        })
        .await;
        let rendered = fixture.mode.chat_render_text();
        assert!(
            rendered.contains(
                "Saved API key for Stalled Login, but its model catalog refresh timed out; using cached models."
            ),
            "{rendered}"
        );
    })
    .await;
    unsafe {
        std::env::remove_var("KIANA_TEST_STALLED_LOGIN_KEY");
        std::env::set_var("KIANA_OFFLINE", "1");
    }
}

// ---------------------------------------------------------------------------
// review 回归：`getAuthCredential` 的大小写不敏感头查找与 Bearer 解析
// ---------------------------------------------------------------------------

fn auth_result(
    api_key: Option<&str>,
    headers: &[(&str, &str)],
) -> kiana_ai::auth::types::AuthResult {
    let mut map = indexmap::IndexMap::new();
    for (name, value) in headers {
        map.insert((*name).to_string(), Some((*value).to_string()));
    }
    kiana_ai::auth::types::AuthResult {
        auth: kiana_ai::auth::types::ModelAuth {
            api_key: api_key.map(str::to_string),
            headers: if map.is_empty() { None } else { Some(map) },
            base_url: None,
        },
        env: None,
        source: None,
    }
}

#[test]
fn auth_credential_matches_ts_bearer_header_semantics() {
    // 空 apiKey 是 falsy：回退到 headers。
    assert_eq!(
        InteractiveMode::test_get_auth_credential(&auth_result(
            Some(""),
            &[("authorization", "Bearer token-1")]
        )),
        Some("token-1".to_string())
    );
    // 头名大小写不敏感（TS：`name.toLowerCase() === "authorization"`）。
    assert_eq!(
        InteractiveMode::test_get_auth_credential(&auth_result(
            None,
            &[("AUTHORIZATION", "Bearer token-2")]
        )),
        Some("token-2".to_string())
    );
    // `/^Bearer\s+(.+)$/iu`：前缀大小写不敏感、任意空白、其余全部捕获。
    assert_eq!(
        InteractiveMode::test_get_auth_credential(&auth_result(
            None,
            &[("Authorization", "bEaReR \t  token 3")]
        )),
        Some("token 3".to_string())
    );
    assert_eq!(
        InteractiveMode::test_get_auth_credential(&auth_result(
            None,
            &[("Authorization", "Basic abc")]
        )),
        None
    );
    // apiKey 优先于 headers。
    assert_eq!(
        InteractiveMode::test_get_auth_credential(&auth_result(
            Some("sk-ant-oat01-test"),
            &[("Authorization", "Bearer ignored")]
        )),
        Some("sk-ant-oat01-test".to_string())
    );
}
