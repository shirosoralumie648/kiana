//! `test/model-selector.test.ts`、`test/scoped-models-selector.test.ts` 与
//! 回归测试 3217 / 6949 / 6999 / 7153 / 7209 的**选择器级**断言。
//!
//! 命令级 / `InteractiveMode` 级断言见 `im-commands` 单元（`showModelSelector` /
//! `showModelsSelector` 的接线）；本文件只覆盖组件本身。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_ai::auth::types::CredentialStore;
use kiana_ai::error::{AiError, AiErrorKind};
use kiana_ai::models::ModelsRefreshResult;
use kiana_ai::types::Model;
use kiana_ai::utils::abort::AbortScope;
use kiana_config::auth::models_store::InMemoryCodingAgentModelsStore;
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::keybindings::keybindings;
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use kiana_interactive::components::model_selector::{
    DefaultModelReference, ModelSelectorComponent, ModelSelectorRuntime, ScopedModelItem,
};
use kiana_interactive::components::scoped_models_selector::{
    ModelsCallbacks, ModelsConfig, RefreshStatusKind, ScopedModelsSelectorComponent,
};
use kiana_interactive::model_catalog_refresh::{ModelCatalogRefreshFuture, ModelCatalogRuntime};
use kiana_tui::keybindings::KeyIdList;
use kiana_tui::tui::{
    Component, MouseFocusTarget, TuiMouseButton, TuiMouseEvent, TuiMouseEventType,
    dispatch_mouse_event,
};
use kiana_tui::{KeybindingsConfig, KeybindingsManager, set_keybindings, strip_terminal_sequences};
use serde_json::json;

// ---------------------------------------------------------------------------
// 公共设施
// ---------------------------------------------------------------------------

/// 初始化 dark 主题（进程级一次）并安装包含 `app.models.*` 的键位表（thread-local）。
fn setup(user_bindings: KeybindingsConfig) {
    static THEME_ONCE: std::sync::Once = std::sync::Once::new();
    THEME_ONCE.call_once(|| kiana_theme::init_theme(Some("dark"), false));
    set_keybindings(KeybindingsManager::new(
        keybindings().clone(),
        user_bindings,
    ));
}

fn model(provider: &str, id: &str, name: &str) -> Model {
    Model {
        id: id.to_string(),
        name: name.to_string(),
        provider: provider.to_string(),
        ..Default::default()
    }
}

fn render(component: &mut dyn Component, width: usize) -> String {
    strip_terminal_sequences(&component.render(width).join("\n"))
}

/// 等待选择器渲染出某个字符串，返回当时的纯文本渲染。
async fn wait_for_render(selector: &mut ModelSelectorComponent, needle: &str) -> String {
    for _ in 0..10_000 {
        let rendered = render(selector, 120);
        if rendered.contains(needle) {
            return rendered;
        }
        tokio::task::yield_now().await;
    }
    panic!("never rendered {needle}");
}

fn model_row(rendered: &str, id: &str) -> Option<String> {
    rendered
        .split('\n')
        .find(|line| line.contains(&format!("{id} [")))
        .map(|line| line.trim_end().to_string())
}

/// TS `selectedModelId`：找到以 `→ ` 开头的行并取模型 id。
fn selected_model_id(rendered: &str) -> Option<String> {
    let line = rendered.split('\n').find(|line| line.starts_with("→ "))?;
    let rest = line.strip_prefix("→")?.trim_start();
    let rest = rest.strip_prefix('✓').map(str::trim_start).unwrap_or(rest);
    rest.split(' ')
        .next()
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// 解析列表滚动指示器 `  (k/N)` 里的总数 N；没有指示器（列表被完整显示）时返回 None。
fn scroll_total(rendered: &str) -> Option<usize> {
    rendered.lines().find_map(|line| {
        let rest = line.trim().strip_prefix('(')?.strip_suffix(')')?;
        let (selected, total) = rest.split_once('/')?;
        selected.parse::<usize>().ok()?;
        total.parse().ok()
    })
}

/// TS `getMarkerStates`：按 `line.slice(2).startsWith("✓ ")` 判断。
fn marker_states(selector: &mut ScopedModelsSelectorComponent, ids: &[String]) -> Vec<bool> {
    let rendered = render(selector, 120);
    let lines: Vec<&str> = rendered.split('\n').collect();
    ids.iter()
        .map(|id| {
            let line = lines
                .iter()
                .find(|line| line.contains(&format!("{id} [")))
                .unwrap_or_else(|| panic!("Expected rendered row for {id}"));
            line.chars().skip(2).collect::<String>().starts_with("✓ ")
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 假运行时（替代 TS 的 vi.spyOn(ModelRuntime, ...)）
// ---------------------------------------------------------------------------

struct FakeState {
    id: usize,
    snapshot: Vec<Model>,
    error: Option<String>,
    result: Result<ModelsRefreshResult, AiError>,
    /// `refresh_catalogs` 已开始执行（进入共享刷新任务）。
    started: bool,
    /// 让 `refresh_catalogs` 等到共享 `AbortScope` 被中止后才返回。
    await_abort: bool,
    /// `await_abort` 模式下观察到了中止。
    aborted: bool,
}

#[derive(Clone)]
struct FakeRuntime {
    state: Arc<Mutex<FakeState>>,
}

impl FakeRuntime {
    fn new(snapshot: Vec<Model>) -> Self {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
        FakeRuntime {
            state: Arc::new(Mutex::new(FakeState {
                id: NEXT_ID.fetch_add(1, Ordering::SeqCst),
                snapshot,
                error: None,
                result: Ok(ModelsRefreshResult::default()),
                started: false,
                await_abort: false,
                aborted: false,
            })),
        }
    }

    fn with_result(self, result: Result<ModelsRefreshResult, AiError>) -> Self {
        self.state.lock().unwrap().result = result;
        self
    }

    fn with_error(self, error: Option<&str>) -> Self {
        self.state.lock().unwrap().error = error.map(str::to_string);
        self
    }

    fn with_await_abort(self) -> Self {
        self.state.lock().unwrap().await_abort = true;
        self
    }

    fn started(&self) -> bool {
        self.state.lock().unwrap().started
    }

    fn aborted(&self) -> bool {
        self.state.lock().unwrap().aborted
    }
}

impl ModelCatalogRuntime for FakeRuntime {
    fn identity(&self) -> usize {
        self.state.lock().unwrap().id
    }

    fn refresh_catalogs(&self, signal: Option<AbortScope>) -> ModelCatalogRefreshFuture<'_> {
        let await_abort = {
            let mut state = self.state.lock().unwrap();
            state.started = true;
            state.await_abort
        };
        Box::pin(async move {
            if await_abort {
                if let Some(signal) = signal {
                    signal.cancelled().await;
                }
                self.state.lock().unwrap().aborted = true;
            }
            self.state.lock().unwrap().result.clone()
        })
    }
}

impl ModelSelectorRuntime for FakeRuntime {
    fn get_available_snapshot(&self) -> Vec<Model> {
        self.state.lock().unwrap().snapshot.clone()
    }

    fn get_model(&self, provider_id: &str, model_id: &str) -> Option<Model> {
        self.state
            .lock()
            .unwrap()
            .snapshot
            .iter()
            .find(|model| model.provider == provider_id && model.id == model_id)
            .cloned()
    }

    fn get_error(&self) -> Option<String> {
        self.state.lock().unwrap().error.clone()
    }
}

fn noop_select() -> Box<dyn FnMut(Model)> {
    Box::new(|_| {})
}

fn noop_cancel() -> Box<dyn FnMut()> {
    Box::new(|| {})
}

// ---------------------------------------------------------------------------
// model-selector.test.ts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn keeps_the_current_model_marked_while_browsing() {
    setup(KeybindingsConfig::new());
    tokio::task::LocalSet::new()
        .run_until(async {
            let runtime = FakeRuntime::new(vec![
                model("faux", "current-model", "Current Model"),
                model("faux", "browsed-model", "Browsed Model"),
            ]);
            let current_model = runtime.get_model("faux", "current-model");
            let mut selector = ModelSelectorComponent::new(
                None,
                current_model,
                runtime,
                Vec::new(),
                noop_select(),
                noop_cancel(),
                None,
                None,
                None,
            );

            let rendered = wait_for_render(&mut selector, "Model catalogs refreshed.").await;
            assert_eq!(
                model_row(&rendered, "current-model").as_deref(),
                Some("→ ✓ current-model [faux]")
            );
            selector.handle_input("\x1b[B");
            let rendered = render(&mut selector, 120);
            assert_eq!(
                model_row(&rendered, "current-model").as_deref(),
                Some("  ✓ current-model [faux]")
            );
            assert_eq!(
                model_row(&rendered, "browsed-model").as_deref(),
                Some("→   browsed-model [faux]")
            );
            selector.dispose();
        })
        .await;
}

// #5916
#[tokio::test]
async fn shows_model_names_that_differ_from_the_id() {
    setup(KeybindingsConfig::new());
    tokio::task::LocalSet::new()
        .run_until(async {
            let runtime = FakeRuntime::new(vec![
                model("faux", "minimax/minimax-m3", "MiniMax M3 [parasail]"),
                model("faux", "claude-sonnet-4-5", "Claude Sonnet 4.5"),
            ]);
            let mut selector = ModelSelectorComponent::new(
                None,
                None,
                runtime,
                Vec::new(),
                noop_select(),
                noop_cancel(),
                None,
                None,
                None,
            );
            let rendered = wait_for_render(&mut selector, "Model catalogs refreshed.").await;
            let rows: Vec<String> = rendered
                .split('\n')
                .map(|line| {
                    line.chars()
                        .skip(4)
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect();
            assert!(
                rows.contains(&"minimax/minimax-m3 (MiniMax M3 [parasail]) [faux]".to_string())
            );
            assert!(rows.contains(&"claude-sonnet-4-5 [faux]".to_string()));
            selector.dispose();
        })
        .await;
}

#[tokio::test]
async fn uses_the_configured_save_binding() {
    let mut user = KeybindingsConfig::new();
    user.insert(
        "app.models.save".to_string(),
        KeyIdList::One("ctrl+r".to_string()),
    );
    setup(user);
    tokio::task::LocalSet::new()
        .run_until(async {
            let runtime = FakeRuntime::new(vec![model("faux", "current-model", "Current Model")]);
            let current_model = runtime.get_model("faux", "current-model");
            let saved: Rc<RefCell<Vec<Model>>> = Rc::new(RefCell::new(Vec::new()));
            let saved_in_callback = Rc::clone(&saved);
            let mut selector = ModelSelectorComponent::new(
                None,
                current_model.clone(),
                runtime,
                Vec::new(),
                noop_select(),
                noop_cancel(),
                None,
                Some(Box::new(move |model| {
                    saved_in_callback.borrow_mut().push(model);
                })),
                None,
            );

            let rendered = wait_for_render(&mut selector, "Model catalogs refreshed.").await;
            assert!(rendered.contains("Ctrl+R to set as default"));

            selector.handle_input("\x13");
            assert!(saved.borrow().is_empty());
            selector.handle_input("\x12");
            assert_eq!(saved.borrow().len(), 1);
            assert_eq!(saved.borrow()[0].id, "current-model");
            assert_eq!(
                saved.borrow()[0].provider,
                current_model.as_ref().unwrap().provider
            );
        })
        .await;
}

#[tokio::test]
async fn lists_every_catalog_that_failed_to_refresh() {
    setup(KeybindingsConfig::new());
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut errors = IndexMap::new();
            errors.insert(
                "openai".to_string(),
                AiError::new(AiErrorKind::Other, "unavailable"),
            );
            errors.insert(
                "anthropic".to_string(),
                AiError::new(AiErrorKind::Other, "unavailable"),
            );
            let runtime = FakeRuntime::new(vec![model("faux", "m", "M")]).with_result(Ok(
                ModelsRefreshResult {
                    aborted: false,
                    errors,
                },
            ));
            let current_model = runtime.get_model("faux", "m");
            let mut selector = ModelSelectorComponent::new(
                None,
                current_model,
                runtime,
                Vec::new(),
                noop_select(),
                noop_cancel(),
                None,
                None,
                None,
            );
            let rendered = wait_for_render(
                &mut selector,
                "Could not refresh 2 model catalogs (openai, anthropic); showing cached models.",
            )
            .await;
            assert!(rendered.contains(
                "Could not refresh 2 model catalogs (openai, anthropic); showing cached models."
            ));
        })
        .await;
}

// TS `if (!this.errorMessage)`：空串是 falsy，等同于 undefined。
#[tokio::test]
async fn treats_an_empty_refresh_error_as_no_error() {
    setup(KeybindingsConfig::new());
    tokio::task::LocalSet::new()
        .run_until(async {
            let runtime = FakeRuntime::new(vec![model("faux", "m", "M")]).with_error(Some(""));
            let mut selector = ModelSelectorComponent::new(
                None,
                None,
                runtime,
                Vec::new(),
                noop_select(),
                noop_cancel(),
                None,
                None,
                None,
            );
            let rendered = wait_for_render(&mut selector, "Model catalogs refreshed.").await;
            assert!(rendered.contains("Model catalogs refreshed."));
            assert!(rendered.contains("Model Name: M"));
            selector.dispose();

            // 空列表也必须回落到 "  No matching models"，而不是渲染空白的错误行。
            let runtime = FakeRuntime::new(Vec::new()).with_error(Some(""));
            let mut selector = ModelSelectorComponent::new(
                None,
                None,
                runtime,
                Vec::new(),
                noop_select(),
                noop_cancel(),
                None,
                None,
                None,
            );
            let rendered = wait_for_render(&mut selector, "Model catalogs refreshed.").await;
            assert!(rendered.contains("No matching models"));
            selector.dispose();
        })
        .await;
}

// TS 的 dispose() 只置 closed，不清空 onSelect/onCancel/onSelectAsDefault：
// 同一个选择器上的重复按键仍应触发回调。
#[tokio::test]
async fn keeps_host_callbacks_installed_for_repeated_input() {
    setup(KeybindingsConfig::new());
    tokio::task::LocalSet::new()
        .run_until(async {
            let runtime = FakeRuntime::new(vec![model("faux", "m", "M")]);
            let cancels = Rc::new(Cell::new(0usize));
            let selects: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
            let saves: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
            let cancels_in_callback = Rc::clone(&cancels);
            let selects_in_callback = Rc::clone(&selects);
            let saves_in_callback = Rc::clone(&saves);
            let mut selector = ModelSelectorComponent::new(
                None,
                None,
                runtime,
                Vec::new(),
                Box::new(move |model| {
                    selects_in_callback.borrow_mut().push(model.id);
                }),
                Box::new(move || cancels_in_callback.set(cancels_in_callback.get() + 1)),
                None,
                Some(Box::new(move |model| {
                    saves_in_callback.borrow_mut().push(model.id);
                })),
                None,
            );
            wait_for_render(&mut selector, "Model catalogs refreshed.").await;

            selector.handle_input("\x1b");
            selector.handle_input("\x1b");
            assert_eq!(cancels.get(), 2);

            selector.handle_input("\r");
            selector.handle_input("\r");
            assert_eq!(
                selects.borrow().clone(),
                vec!["m".to_string(), "m".to_string()]
            );

            selector.handle_input("\x13");
            selector.handle_input("\x13");
            assert_eq!(
                saves.borrow().clone(),
                vec!["m".to_string(), "m".to_string()]
            );
            selector.dispose();
        })
        .await;
}

// ---------------------------------------------------------------------------
// 3217（选择器级）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preserves_scoped_model_order_in_the_model_scoped_tab() {
    setup(KeybindingsConfig::new());
    tokio::task::LocalSet::new()
        .run_until(async {
            let runtime = FakeRuntime::new(vec![
                model("faux", "faux-1", "One"),
                model("faux", "faux-2", "Two"),
                model("faux", "faux-3", "Three"),
            ]);
            let one = runtime.get_model("faux", "faux-1").unwrap();
            let two = runtime.get_model("faux", "faux-2").unwrap();
            let three = runtime.get_model("faux", "faux-3").unwrap();
            let mut selector = ModelSelectorComponent::new(
                None,
                Some(one.clone()),
                runtime,
                vec![
                    ScopedModelItem {
                        model: two,
                        thinking_level: None,
                    },
                    ScopedModelItem {
                        model: one,
                        thinking_level: None,
                    },
                    ScopedModelItem {
                        model: three,
                        thinking_level: None,
                    },
                ],
                noop_select(),
                noop_cancel(),
                None,
                None,
                None,
            );
            let rendered = wait_for_render(&mut selector, "Model catalogs refreshed.").await;
            assert!(rendered.contains("[faux]"));
            let ordered: Vec<String> = rendered
                .split('\n')
                .filter(|line| line.contains("[faux]"))
                .take(3)
                .map(|line| {
                    let mut rest = line.trim();
                    if let Some(stripped) = rest.strip_prefix('→') {
                        rest = stripped.trim_start();
                    }
                    if let Some(stripped) = rest.strip_prefix('✓') {
                        rest = stripped.trim_start();
                    }
                    rest.split(' ').next().unwrap_or("").to_string()
                })
                .collect();
            assert_eq!(ordered, vec!["faux-2", "faux-1", "faux-3"]);
        })
        .await;
}

// ---------------------------------------------------------------------------
// 6999：models.json 热重载（选择器打开时刷新）
// ---------------------------------------------------------------------------

fn models_json(provider: &str, model: &str) -> String {
    json!({
        "providers": {
            provider: {
                "baseUrl": "https://example.test/v1",
                "api": "openai-completions",
                "apiKey": "test-key",
                "models": [{ "id": model }],
            }
        }
    })
    .to_string()
}

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(1);
        let path = std::env::temp_dir().join(format!(
            "kiana-model-selector-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn reloads_models_json_when_opening_the_model_selector() {
    setup(KeybindingsConfig::new());
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp_dir = TempDir::new();
            let models_path = temp_dir.path().join("models.json");
            std::fs::write(&models_path, models_json("old-provider", "old-model")).unwrap();
            let auth_storage = Arc::new(AuthStorage::in_memory(auth_data_from_value(json!({}))));
            let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
                credentials: Some(auth_storage as Arc<dyn CredentialStore>),
                models_path: ModelsPathOption::Path(models_path.to_string_lossy().into_owned()),
                models_store: Some(Arc::new(InMemoryCodingAgentModelsStore::new())),
                allow_model_network: false,
                ..Default::default()
            })
            .await
            .unwrap();
            assert!(runtime.get_model("old-provider", "old-model").is_some());

            std::fs::write(&models_path, models_json("new-provider", "new-model")).unwrap();
            let runtime_for_assertions = runtime.clone();
            let renders = Rc::new(Cell::new(0usize));
            let renders_in_callback = Rc::clone(&renders);
            let mut selector = ModelSelectorComponent::new(
                Some(Rc::new(move || {
                    renders_in_callback.set(renders_in_callback.get() + 1);
                })),
                None,
                runtime,
                Vec::new(),
                noop_select(),
                noop_cancel(),
                None,
                None,
                None,
            );
            // 真实 ModelRuntime 的刷新会做本地可用性检查（可能等待 HTTP 超时），因此按墙钟等待。
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while renders.get() < 2 && std::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            assert!(renders.get() >= 2, "render count {}", renders.get());

            // 直接检查刷新后的运行时快照：「旧模型消失」必须能在未过滤的数据上成立，
            // 否则输入 "new-model" 之后的 `!contains(old-model)` 只是被搜索条件掩盖。
            let snapshot = runtime_for_assertions.get_available_snapshot();
            assert!(
                snapshot
                    .iter()
                    .any(|model| model.provider == "new-provider" && model.id == "new-model"),
                "refreshed snapshot is missing new-model: {snapshot:?}"
            );
            assert!(
                !snapshot
                    .iter()
                    .any(|model| model.provider == "old-provider" && model.id == "old-model"),
                "refreshed snapshot still contains old-model: {snapshot:?}"
            );

            // TS 在**未过滤**列表上断言新模型存在、旧模型消失（6999:77-81）。测试机可能带有
            // 真实 provider 的环境凭证（列表超过 maxVisible=10 时只显示前 10 行），因此：
            // - 列表能完整显示（没有滚动指示器）→ 做与 TS 相同的正/负断言；
            // - 有滚动指示器 `(k/N)` → 比较 N 与刷新后的快照长度：若选择器仍残留旧模型，
            //   N 会比快照多 1（负向断言在过滤/截断下会被掩盖，这里用总数兜底）。
            let unfiltered = render(&mut selector, 120);
            match scroll_total(&unfiltered) {
                Some(total) => assert_eq!(
                    total,
                    snapshot.len(),
                    "selector list ({total}) diverged from the refreshed snapshot ({}):\n{unfiltered}",
                    snapshot.len()
                ),
                None => {
                    assert!(
                        unfiltered.contains("new-model [new-provider]"),
                        "unfiltered render is missing the new model:\n{unfiltered}"
                    );
                    assert!(
                        !unfiltered.contains("old-model [old-provider]"),
                        "unfiltered render still contains the old model:\n{unfiltered}"
                    );
                }
            }

            // 搜索路径仍能找到刷新后的新模型（TS 的 harness 没有内置模型，直接看默认列表）。
            for character in "new-model".chars() {
                selector.handle_input(&character.to_string());
            }
            let rendered = render(&mut selector, 120);
            assert!(rendered.contains("new-model [new-provider]"));
            selector.dispose();
        })
        .await;
}

// ---------------------------------------------------------------------------
// 7209：过滤把选择移回第一行
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filter_moves_selection_to_the_first_row_in_the_all_tab() {
    setup(KeybindingsConfig::new());
    tokio::task::LocalSet::new()
        .run_until(async {
            let runtime = FakeRuntime::new(vec![
                model("faux", "alpha-1", "Alpha One"),
                model("faux", "alpha-2", "Alpha Two"),
                model("faux", "alpha-3", "Alpha Three"),
                model("faux", "beta-1", "Beta One"),
            ]);
            let current = runtime.get_model("faux", "alpha-1");
            let mut selector = ModelSelectorComponent::new(
                None,
                current,
                runtime,
                Vec::new(),
                noop_select(),
                noop_cancel(),
                None,
                None,
                None,
            );
            wait_for_render(&mut selector, "Model catalogs refreshed.").await;

            assert_eq!(
                selected_model_id(&render(&mut selector, 120)).as_deref(),
                Some("alpha-1")
            );
            selector.handle_input("\x1b[B");
            selector.handle_input("\x1b[B");
            assert_eq!(
                selected_model_id(&render(&mut selector, 120)).as_deref(),
                Some("alpha-3")
            );
            for character in "alpha".chars() {
                selector.handle_input(&character.to_string());
            }
            let rendered = render(&mut selector, 120);
            assert_eq!(selected_model_id(&rendered).as_deref(), Some("alpha-1"));
            assert!(!rendered.contains("beta-1"));
        })
        .await;
}

#[tokio::test]
async fn filter_moves_selection_to_the_first_row_in_the_scoped_tab() {
    setup(KeybindingsConfig::new());
    tokio::task::LocalSet::new()
        .run_until(async {
            let runtime = FakeRuntime::new(vec![
                model("faux", "alpha-1", "Alpha One"),
                model("faux", "alpha-2", "Alpha Two"),
                model("faux", "alpha-3", "Alpha Three"),
            ]);
            let alpha1 = runtime.get_model("faux", "alpha-1").unwrap();
            let alpha2 = runtime.get_model("faux", "alpha-2").unwrap();
            let alpha3 = runtime.get_model("faux", "alpha-3").unwrap();
            let mut selector = ModelSelectorComponent::new(
                None,
                Some(alpha1.clone()),
                runtime,
                vec![
                    ScopedModelItem {
                        model: alpha2,
                        thinking_level: None,
                    },
                    ScopedModelItem {
                        model: alpha3,
                        thinking_level: None,
                    },
                    ScopedModelItem {
                        model: alpha1,
                        thinking_level: None,
                    },
                ],
                noop_select(),
                noop_cancel(),
                None,
                None,
                None,
            );
            wait_for_render(&mut selector, "Model catalogs refreshed.").await;
            assert_eq!(
                selected_model_id(&render(&mut selector, 120)).as_deref(),
                Some("alpha-1")
            );
            for character in "alpha".chars() {
                selector.handle_input(&character.to_string());
            }
            assert_eq!(
                selected_model_id(&render(&mut selector, 120)).as_deref(),
                Some("alpha-2")
            );
        })
        .await;
}

// ---------------------------------------------------------------------------
// scoped-models-selector.test.ts
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ModelState {
    id: String,
    enabled: bool,
}

struct ScopedHarness {
    selector: ScopedModelsSelectorComponent,
    models: Arc<Mutex<Vec<ModelState>>>,
}

fn create_scoped_selector(models: Vec<(&str, &str, bool)>) -> ScopedHarness {
    let provider = "faux";
    let all_models: Vec<Model> = models
        .iter()
        .map(|(id, name, _)| model(provider, id, name))
        .collect();
    let enabled_ids: Vec<String> = models
        .iter()
        .filter(|(_, _, enabled)| *enabled)
        .map(|(id, _, _)| format!("{provider}/{id}"))
        .collect();
    let states = Arc::new(Mutex::new(
        models
            .iter()
            .map(|(id, _, enabled)| ModelState {
                id: (*id).to_string(),
                enabled: *enabled,
            })
            .collect::<Vec<_>>(),
    ));
    let states_in_callback = Arc::clone(&states);
    let selector = ScopedModelsSelectorComponent::new(
        ModelsConfig {
            all_models,
            enabled_model_ids: Some(enabled_ids),
            refresh_status: None,
        },
        ModelsCallbacks {
            on_change: Box::new(move |enabled_model_ids| {
                for state in states_in_callback.lock().unwrap().iter_mut() {
                    state.enabled = match &enabled_model_ids {
                        None => true,
                        Some(ids) => ids.contains(&format!("{provider}/{}", state.id)),
                    };
                }
                Box::pin(async {})
            }),
            on_persist: Box::new(|_| Box::pin(async {})),
            on_cancel: Box::new(|| {}),
        },
    );
    ScopedHarness {
        selector,
        models: states,
    }
}

fn enabled_flags(harness: &ScopedHarness) -> Vec<bool> {
    harness
        .models
        .lock()
        .unwrap()
        .iter()
        .map(|model| model.enabled)
        .collect()
}

fn short_ids(harness: &ScopedHarness) -> Vec<String> {
    harness
        .models
        .lock()
        .unwrap()
        .iter()
        .map(|model| model.id.clone())
        .collect()
}

// #5916
#[tokio::test]
async fn scoped_selector_shows_model_names_that_differ_from_the_id() {
    setup(KeybindingsConfig::new());
    let mut harness = create_scoped_selector(vec![
        ("model-a", "Model A", true),
        ("model-b", "Renamed B", true),
    ]);
    let rows: Vec<String> = render(&mut harness.selector, 120)
        .split('\n')
        .map(|line| {
            line.chars()
                .skip(4)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    assert!(rows.contains(&"model-a [faux]".to_string()));
    assert!(rows.contains(&"model-b (Renamed B) [faux]".to_string()));
}

#[tokio::test]
async fn scoped_selector_marks_every_model_after_enabling_all() {
    setup(KeybindingsConfig::new());
    let mut harness = create_scoped_selector(vec![
        ("model-a", "Model A", true),
        ("model-b", "Model B", false),
        ("model-c", "Model C", false),
    ]);
    harness.selector.handle_input("\x01");
    assert_eq!(enabled_flags(&harness), vec![true, true, true]);
    let ids = short_ids(&harness);
    assert_eq!(
        marker_states(&mut harness.selector, &ids),
        vec![true, true, true]
    );
    assert!(render(&mut harness.selector, 120).contains("all enabled"));
}

#[tokio::test]
async fn scoped_selector_disables_only_the_selected_model_after_enabling_all() {
    setup(KeybindingsConfig::new());
    let mut harness = create_scoped_selector(vec![
        ("model-a", "Model A", true),
        ("model-b", "Model B", false),
        ("model-c", "Model C", false),
    ]);
    harness.selector.handle_input("\x01");
    harness.selector.handle_input("\r");
    assert_eq!(enabled_flags(&harness), vec![false, true, true]);
    let ids = short_ids(&harness);
    assert_eq!(
        marker_states(&mut harness.selector, &ids),
        vec![false, true, true]
    );
}

#[tokio::test]
async fn scoped_selector_enables_only_the_selected_model_after_clearing_all() {
    setup(KeybindingsConfig::new());
    let mut harness = create_scoped_selector(vec![
        ("model-a", "Model A", true),
        ("model-b", "Model B", true),
        ("model-c", "Model C", true),
    ]);
    harness.selector.handle_input("\x18");
    assert_eq!(enabled_flags(&harness), vec![false, false, false]);
    let ids = short_ids(&harness);
    assert_eq!(
        marker_states(&mut harness.selector, &ids),
        vec![false, false, false]
    );
    harness.selector.handle_input("\r");
    assert_eq!(enabled_flags(&harness), vec![true, false, false]);
    assert_eq!(
        marker_states(&mut harness.selector, &ids),
        vec![true, false, false]
    );
}

#[tokio::test]
async fn scoped_selector_restores_all_enabled_after_re_enabling_the_last_model() {
    setup(KeybindingsConfig::new());
    let mut harness = create_scoped_selector(vec![
        ("model-a", "Model A", true),
        ("model-b", "Model B", false),
        ("model-c", "Model C", false),
    ]);
    harness.selector.handle_input("\x01"); // enable all -> null
    harness.selector.handle_input("\r"); // disable model-a; enabled models re-sort first: [b, c, a]
    harness.selector.handle_input("\x1b[B");
    harness.selector.handle_input("\x1b[B"); // move selection back to model-a
    harness.selector.handle_input("\r"); // re-enable model-a
    assert_eq!(enabled_flags(&harness), vec![true, true, true]);
    let ids = short_ids(&harness);
    assert_eq!(
        marker_states(&mut harness.selector, &ids),
        vec![true, true, true]
    );
    assert!(render(&mut harness.selector, 120).contains("all enabled"));
}

// ---------------------------------------------------------------------------
// 3217：重排写回（选择器级）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scoped_selector_propagates_reordered_models_back_to_the_session_state() {
    setup(KeybindingsConfig::new());
    let provider = "faux";
    let models = vec![
        model(provider, "faux-1", "One"),
        model(provider, "faux-2", "Two"),
        model(provider, "faux-3", "Three"),
    ];
    let ordered_ids: Vec<String> = models
        .iter()
        .map(|model| format!("{provider}/{}", model.id))
        .collect();
    let changes: Arc<Mutex<Vec<Option<Vec<String>>>>> = Arc::new(Mutex::new(Vec::new()));
    let changes_in_callback = Arc::clone(&changes);
    let mut selector = ScopedModelsSelectorComponent::new(
        ModelsConfig {
            all_models: models,
            enabled_model_ids: Some(ordered_ids.clone()),
            refresh_status: None,
        },
        ModelsCallbacks {
            on_change: Box::new(move |enabled_model_ids| {
                changes_in_callback.lock().unwrap().push(enabled_model_ids);
                Box::pin(async {})
            }),
            on_persist: Box::new(|_| Box::pin(async {})),
            on_cancel: Box::new(|| {}),
        },
    );

    selector.handle_input("\x1b[1;3B");
    assert_eq!(
        changes.lock().unwrap().clone(),
        vec![Some(vec![
            ordered_ids[1].clone(),
            ordered_ids[0].clone(),
            ordered_ids[2].clone()
        ])]
    );
}

// ---------------------------------------------------------------------------
// 6949：不可用模型仍可见 / 可移除（选择器级）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scoped_selector_shows_and_removes_an_enabled_model_without_a_catalog_entry() {
    setup(KeybindingsConfig::new());
    let provider = "faux";
    let available = model(provider, "available", "Available");
    let available_id = format!("{provider}/available");
    let unavailable_id = format!("{provider}/unavailable");
    let changes: Arc<Mutex<Vec<Option<Vec<String>>>>> = Arc::new(Mutex::new(Vec::new()));
    let persisted: Arc<Mutex<Vec<Option<Vec<String>>>>> = Arc::new(Mutex::new(Vec::new()));
    let changes_in_callback = Arc::clone(&changes);
    let persisted_in_callback = Arc::clone(&persisted);
    let mut selector = ScopedModelsSelectorComponent::new(
        ModelsConfig {
            all_models: vec![available],
            enabled_model_ids: Some(vec![unavailable_id.clone(), available_id.clone()]),
            refresh_status: None,
        },
        ModelsCallbacks {
            on_change: Box::new(move |enabled_model_ids| {
                changes_in_callback.lock().unwrap().push(enabled_model_ids);
                Box::pin(async {})
            }),
            on_persist: Box::new(move |enabled_model_ids| {
                persisted_in_callback
                    .lock()
                    .unwrap()
                    .push(enabled_model_ids);
                Box::pin(async {})
            }),
            on_cancel: Box::new(|| {}),
        },
    );

    let rendered = render(&mut selector, 100);
    assert!(rendered.contains(&format!("{unavailable_id} [unavailable]")));
    let raw = selector.render(100).join("\n");
    assert!(raw.contains(&kiana_theme::THEME.strikethrough(&unavailable_id).unwrap()));

    selector.handle_input("\r");
    assert_eq!(
        changes.lock().unwrap().clone(),
        vec![Some(vec![available_id.clone()])]
    );
    selector.handle_input("\x13");
    assert_eq!(
        persisted.lock().unwrap().clone(),
        vec![Some(vec![available_id])]
    );
}

// ---------------------------------------------------------------------------
// 7153：scoped 选择器的缓存渲染与刷新状态（选择器级）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scoped_selector_renders_cached_models_then_updates_after_refresh() {
    setup(KeybindingsConfig::new());
    let provider = "faux";
    let cached = model(provider, "cached", "Cached");
    let refreshed = model(provider, "refreshed", "Refreshed");
    let mut selector = ScopedModelsSelectorComponent::new(
        ModelsConfig {
            all_models: vec![cached.clone()],
            enabled_model_ids: Some(vec![format!("{provider}/cached")]),
            refresh_status: Some("Refreshing model catalogs…".to_string()),
        },
        ModelsCallbacks {
            on_change: Box::new(|_| Box::pin(async {})),
            on_persist: Box::new(|_| Box::pin(async {})),
            on_cancel: Box::new(|| {}),
        },
    );
    let initial = render(&mut selector, 100);
    assert!(initial.contains("cached"));
    assert!(initial.contains("Refreshing model catalogs…"));
    assert!(!initial.contains("refreshed"));

    selector.update_models(&[cached, refreshed], None);
    selector.set_refresh_status("Model catalogs refreshed.", RefreshStatusKind::Success);
    let rendered = render(&mut selector, 100);
    assert!(rendered.contains("refreshed"));
    assert!(rendered.contains("Model catalogs refreshed."));
}

// TS 的 `if (config.refreshStatus)`：空串是 falsy，不创建状态行，setRefreshStatus 也就是 no-op。
#[tokio::test]
async fn scoped_selector_treats_empty_refresh_status_as_absent() {
    setup(KeybindingsConfig::new());
    let models = vec![model("faux", "model-a", "Model A")];
    let callbacks = || ModelsCallbacks {
        on_change: Box::new(|_| Box::pin(async {})),
        on_persist: Box::new(|_| Box::pin(async {})),
        on_cancel: Box::new(|| {}),
    };
    let mut with_empty = ScopedModelsSelectorComponent::new(
        ModelsConfig {
            all_models: models.clone(),
            enabled_model_ids: None,
            refresh_status: Some(String::new()),
        },
        callbacks(),
    );
    let mut without = ScopedModelsSelectorComponent::new(
        ModelsConfig {
            all_models: models,
            enabled_model_ids: None,
            refresh_status: None,
        },
        callbacks(),
    );
    assert_eq!(render(&mut with_empty, 50), render(&mut without, 50));

    with_empty.set_refresh_status("Model catalogs refreshed.", RefreshStatusKind::Success);
    let rendered = render(&mut with_empty, 50);
    assert!(!rendered.contains("Model catalogs refreshed."));
    assert_eq!(rendered, render(&mut without, 50));
}

// 过滤器激活时重排会让 TS 的 selectedIndex 变成 -1，随后 updateList 读取
// `filteredItems[-1].model` 抛 TypeError，`notifyChange()` 不会执行
// （scoped-models-selector.ts:302-318、269-275）。Rust 侧保留同一行为。
#[test]
fn scoped_selector_reorder_with_active_filter_throws_like_ts() {
    setup(KeybindingsConfig::new());
    let provider = "faux";
    let models = vec![
        model(provider, "beta", "Beta"),
        model(provider, "alpha", "Alpha"),
        model(provider, "charlie", "Charlie"),
    ];
    let ordered_ids: Vec<String> = ["beta", "alpha", "charlie"]
        .iter()
        .map(|id| format!("{provider}/{id}"))
        .collect();
    let changes: Arc<Mutex<Vec<Option<Vec<String>>>>> = Arc::new(Mutex::new(Vec::new()));
    let changes_in_callback = Arc::clone(&changes);
    let mut selector = ScopedModelsSelectorComponent::new(
        ModelsConfig {
            all_models: models,
            enabled_model_ids: Some(ordered_ids),
            refresh_status: None,
        },
        ModelsCallbacks {
            on_change: Box::new(move |enabled_model_ids| {
                changes_in_callback.lock().unwrap().push(enabled_model_ids);
                Box::pin(async {})
            }),
            on_persist: Box::new(|_| Box::pin(async {})),
            on_cancel: Box::new(|| {}),
        },
    );

    // "ph" 只匹配 Alpha（enabled 下标 1），过滤后的第一行是 alpha。
    for character in "ph".chars() {
        selector.handle_input(&character.to_string());
    }
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        selector.handle_input("\x1b[1;3A"); // alt+up
    }));
    let panic = outcome.expect_err("TS throws TypeError for filteredItems[-1].model");
    let message = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("");
    assert!(
        message.contains("Cannot read properties of undefined"),
        "unexpected panic message: {message}"
    );

    // TS 在抛错前已经写入 enabledIds/isDirty，但 `notifyChange()` 位于 `refresh()` 之后，
    // 因此宿主回调不会被调用。
    assert!(changes.lock().unwrap().is_empty());

    // selectedIndex 保持 -1：下一次渲染同样抛错。
    let rerender = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| selector.render(120)));
    assert!(rerender.is_err(), "selectedIndex stays -1 after the throw");
}

// ---------------------------------------------------------------------------
// default model 标记 / 保存默认模型的引用（补充 model-selector 的构造参数）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn marks_the_default_model_with_a_badge() {
    setup(KeybindingsConfig::new());
    tokio::task::LocalSet::new()
        .run_until(async {
            let runtime = FakeRuntime::new(vec![
                model("faux", "model-a", "Model A"),
                model("faux", "model-b", "Model B"),
            ]);
            let current = runtime.get_model("faux", "model-a");
            let mut selector = ModelSelectorComponent::new(
                None,
                current,
                runtime,
                Vec::new(),
                noop_select(),
                noop_cancel(),
                None,
                None,
                Some(DefaultModelReference {
                    provider: "faux".to_string(),
                    id: "model-b".to_string(),
                }),
            );
            let rendered = wait_for_render(&mut selector, "Model catalogs refreshed.").await;
            let row = model_row(&rendered, "model-b").unwrap();
            assert!(row.ends_with(" · default"));
        })
        .await;
}

// ---------------------------------------------------------------------------
// 鼠标转发（两个组件在 TS 里都继承 Container，点击搜索行应交给 Input）
// ---------------------------------------------------------------------------

fn mouse_press(x: isize, y: isize, width: usize, height: usize) -> TuiMouseEvent {
    TuiMouseEvent {
        event_type: TuiMouseEventType::Press,
        button: TuiMouseButton::Left,
        x,
        y,
        screen_x: x,
        screen_y: y,
        width,
        height,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: None,
    }
}

/// 在渲染结果里找到包含 `needle` 的行号。
fn line_index(rendered: &str, needle: &str) -> isize {
    rendered
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no rendered line contains {needle:?}")) as isize
}

#[test]
fn scoped_selector_forwards_mouse_clicks_to_the_search_input() {
    setup(KeybindingsConfig::new());
    let mut harness = create_scoped_selector(vec![("model-a", "Model A", true)]);
    for character in "abcd".chars() {
        harness.selector.handle_input(&character.to_string());
    }
    let rendered = render(&mut harness.selector, 120);
    let y = line_index(&rendered, "abcd");
    let height = rendered.lines().count();
    let address = &harness.selector as *const ScopedModelsSelectorComponent as usize;

    let result = dispatch_mouse_event(&mut harness.selector, &mouse_press(2, y, 120, height))
        .expect("click on the search line must be handled");
    assert_eq!(result.focus, Some(true));
    assert_eq!(
        result.focus_target,
        Some(MouseFocusTarget::Address(address))
    );

    // Input.handleMouse：x - 2 列 → 文本开头，随后键入的字符插在光标处。
    assert_eq!(harness.selector.get_search_input().cursor(), 0);
    harness.selector.handle_input("X");
    assert_eq!(harness.selector.get_search_input().get_value(), "Xabcd");
}

#[tokio::test]
async fn model_selector_forwards_mouse_clicks_to_the_search_input() {
    setup(KeybindingsConfig::new());
    tokio::task::LocalSet::new()
        .run_until(async {
            let runtime = FakeRuntime::new(vec![model("faux", "m", "M")]);
            let mut selector = ModelSelectorComponent::new(
                None,
                None,
                runtime,
                Vec::new(),
                noop_select(),
                noop_cancel(),
                None,
                None,
                None,
            );
            wait_for_render(&mut selector, "Model catalogs refreshed.").await;
            for character in "abcd".chars() {
                selector.handle_input(&character.to_string());
            }
            let rendered = render(&mut selector, 120);
            let y = line_index(&rendered, "abcd");
            let height = rendered.lines().count();
            let address = &selector as *const ModelSelectorComponent as usize;

            let result = dispatch_mouse_event(&mut selector, &mouse_press(2, y, 120, height))
                .expect("click on the search line must be handled");
            assert_eq!(result.focus, Some(true));
            assert_eq!(
                result.focus_target,
                Some(MouseFocusTarget::Address(address))
            );

            assert_eq!(selector.get_search_input().cursor(), 0);
            selector.handle_input("X");
            assert_eq!(selector.get_search_input().get_value(), "Xabcd");
            selector.dispose();
        })
        .await;
}

// ---------------------------------------------------------------------------
// TS `showSelector` 的 dispose 回调：把选择器装箱后仍必须能中止后台刷新。
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispose_is_reachable_through_the_component_trait_and_aborts_the_refresh() {
    setup(KeybindingsConfig::new());
    tokio::task::LocalSet::new()
        .run_until(async {
            let runtime = FakeRuntime::new(vec![model("faux", "m", "M")]).with_await_abort();
            let runtime_for_wait = runtime.clone();
            let mut boxed: Box<dyn Component> = Box::new(ModelSelectorComponent::new(
                None,
                None,
                runtime,
                Vec::new(),
                noop_select(),
                noop_cancel(),
                None,
                None,
                None,
            ));

            for _ in 0..10_000 {
                if runtime_for_wait.started() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(
                runtime_for_wait.started(),
                "background refresh never started"
            );

            let selector = boxed
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<ModelSelectorComponent>())
                .expect("ModelSelectorComponent must be reachable through Component::as_any_mut");
            selector.dispose();
            selector.dispose(); // 幂等

            for _ in 0..10_000 {
                if runtime_for_wait.aborted() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(
                runtime_for_wait.aborted(),
                "dispose must abort the shared catalog refresh (TS disposeActiveSelector)"
            );
        })
        .await;
}

// ---------------------------------------------------------------------------
// 回调不要求 Send/Sync：TS 的 ModelsCallbacks 是普通闭包（宿主可以捕获 Rc<RefCell<…>>）。
// ---------------------------------------------------------------------------

#[test]
fn scoped_selector_callbacks_can_capture_non_send_state() {
    setup(KeybindingsConfig::new());
    let changes = Rc::new(Cell::new(0usize));
    let changes_in_callback = Rc::clone(&changes);
    let mut selector = ScopedModelsSelectorComponent::new(
        ModelsConfig {
            all_models: vec![model("faux", "model-a", "Model A")],
            enabled_model_ids: None,
            refresh_status: None,
        },
        ModelsCallbacks {
            on_change: Box::new(move |_| {
                changes_in_callback.set(changes_in_callback.get() + 1);
                Box::pin(async {})
            }),
            on_persist: Box::new(|_| Box::pin(async {})),
            on_cancel: Box::new(|| {}),
        },
    );
    selector.handle_input("\r");
    assert_eq!(changes.get(), 1);
}
