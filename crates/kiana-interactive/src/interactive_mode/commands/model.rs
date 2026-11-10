//! `interactive-mode.ts` 的 `/model` 与 `/scoped-models`（§2.50.11）。
//!
//! `handleModelCommand` / `findExactModelMatch` / `showModelSelector` / `showModelsSelector`。
//!
//! # 为什么需要这个文件
//!
//! `/model` 与 `/scoped-models` 是用户切换/圈定模型的两条入口：前者接受一个可选的搜索词
//! （精确命中就直接切换，否则打开模型选择器），后者打开一个可多选、可持久化到 settings 的
//! 模型圈选界面。两处共享同一批底层能力——`ModelRuntime` 的可用模型快照、目录刷新
//! （`crate::model_catalog_refresh`，带 15 秒超时）与模型引用精确匹配
//! （`kiana-config` 的 `find_exact_model_reference_match`）——本文件把它们接到交互模式的
//! 选择器基础设施（`commands::mod` 的 `show_selector`）上。
//!
//! # 两条命令的执行流程
//!
//! - `/model`：`handle_model_command` → 无参数或有参数但未精确命中时打开
//!   [`ModelSelectorComponent`]；精确命中时直接 `set_model`（`persist: false`，不写入
//!   settings 的默认模型）并刷新 footer/边框。
//! - `/scoped-models`：`show_models_selector` 构造共享状态（可用模型列表、启用 id 集合、
//!   当前启用集合）交给 [`ScopedModelsSelectorComponent`]，选择器的变更回调即时写回会话的
//!   scoped models，持久化回调写入 settings 的 `enabledModels`；同时后台跑一次目录刷新，
//!   完成后把新快照推进选择器与会话。
//!
//! # 注意事项
//!
//! - 本文件大量使用 `Rc<RefCell<…>>` 共享状态 + `tokio::task::spawn_local`。所有回调只捕获
//!   `Weak`（经 `InteractiveMode::downgrade`），模式释放后升级失败即静默跳过；这是
//!   `commands/mod.rs` 模块文档所述「延后一拍」纪律在命令层的延续。
//! - 刷新续体必须在动手前检查 `disposed` 标志：选择器可能早已关闭（tests/
//!   `interactive_mode_model_auth.rs` 锁定该行为），迟到的刷新不得再改会话状态或请求重绘。
//! - `/model` 的精确命中不得触发目录刷新（同测试文件锁定：缓存命中时状态行保持为空）；
//!   只有缓存未命中且没有 scoped models 时才允许后台刷新。

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use kiana_ai::types::Model;
use kiana_config::models::resolver::{
    find_exact_model_reference_match, resolve_model_scope_from_models,
};
use kiana_config::models::runtime::ModelRuntime;
use tokio_util::sync::CancellationToken;

use crate::components::model_selector::{
    DefaultModelReference, ModelSelectorComponent, ScopedModelItem,
};
use crate::components::scoped_models_selector::{
    ModelsCallbackFuture, ModelsCallbacks, ModelsConfig, RefreshStatusKind,
    ScopedModelsSelectorComponent,
};
use crate::model_catalog_refresh::refresh_model_catalogs;

use super::super::InteractiveMode;
use super::{SelectorCreation, SelectorFocus};

/// `modelKey(model)`：`provider/id`。
///
/// 会话与 settings 里引用一个模型的稳定短键：`/scoped-models` 用它把「当前可用模型」
/// 与「启用集合」做集合比对（可用 id 集合、选择器回传的启用 id 列表都由它生成）。
/// `provider` 可能为空串，此时键形如 `/id`，仍能参与集合运算。
fn model_key(model: &Model) -> String {
    format!("{}/{}", model.provider, model.id)
}

impl InteractiveMode {
    /// `handleModelCommand(searchTerm?)`。
    ///
    /// `/model` 命令入口（`dispatch_submit` 已清空编辑器，由 `spawn_local` 异步调进来）。
    /// 行为分两支：
    ///
    /// - `search_term` 缺失，或精确匹配失败 → 打开模型选择器（有参数时把参数作为选择器的
    ///   初始搜索文本，便于用户看到「为什么没命中」）。
    /// - 精确命中 → 直接切模型，等价于在选择器里选中该项（`persist: false`）。
    ///
    /// 失败路径只有一条：`set_model` 返回 `Err`（例如该 provider 没有 API key），此时
    /// `show_error` 展示错误文本，会话当前模型不变。
    pub(crate) async fn handle_model_command(&mut self, search_term: Option<String>) {
        let Some(search_term) = search_term else {
            self.show_model_selector(None);
            return;
        };

        let model = self.find_exact_model_match(&search_term).await;
        let Some(model) = model else {
            self.show_model_selector(Some(search_term));
            return;
        };

        let result = self
            .session()
            .set_model(
                model.clone(),
                kiana_session::agent_session::ModelMutationOptions {
                    persist: Some(false),
                },
            )
            .await;
        match result {
            Ok(()) => {
                self.inner.borrow().footer.borrow_mut().invalidate();
                self.update_editor_border_color();
                self.show_status(&format!("Model: {}", model.id));
                self.maybe_warn_about_anthropic_subscription_auth_for(Some(model))
                    .await;
            }
            Err(error) => self.show_error(&error.to_string()),
        }
    }

    /// `findExactModelMatch(searchTerm)`。
    ///
    /// 在「当前可用模型」里找与搜索词精确匹配的模型。可用模型集合按优先级取：
    ///
    /// 1. 会话有 scoped models 时只在这个圈里找（`/model` 不越过 scoped 边界）；
    /// 2. 否则用 `ModelRuntime::get_available_snapshot()`（只读快照，不触发刷新）。
    ///
    /// 缓存集合未命中且没有 scoped models 时，才发起一次目录刷新（15 秒超时，见下），
    /// 刷新结束后用新快照重试匹配。刷新的失败与超时都只降级为警告——用缓存模型继续，
    /// 不阻塞 `/model` 命令本身。测试 `tests/interactive_mode_model_auth.rs` 锁定：缓存
    /// 命中时不出现 "Refreshing model catalogs…"，未命中时才出现。
    ///
    /// 返回 `None` 表示两条路径都没命中，调用方应打开选择器。
    async fn find_exact_model_match(&mut self, search_term: &str) -> Option<Model> {
        let session = self.session();
        let scoped_models = session.scoped_models();
        let cached_models: Vec<Model> = if scoped_models.is_empty() {
            session.model_runtime().get_available_snapshot()
        } else {
            scoped_models
                .into_iter()
                .map(|scoped| scoped.model)
                .collect()
        };
        let cached_match = find_exact_model_reference_match(search_term, &cached_models);
        if cached_match.is_some() || !session.scoped_models().is_empty() {
            return cached_match;
        }

        self.show_status("Refreshing model catalogs…");
        // 刷新的等待与 15 秒定时器竞速：超时侧先 cancel token 再 `refresh.await` 等
        // 底层收尾，然后构造 aborted 错误走「降级到缓存」分支。`refresh_model_catalogs`
        // 是进程级共享协调器（见 `crate::model_catalog_refresh`）：取消只结束本等待者，
        // 底层刷新仍可能被其他等待者驱动到结束。
        let signal = CancellationToken::new();
        let runtime = session.model_runtime().clone();
        let refresh = refresh_model_catalogs(&runtime, Some(&signal));
        tokio::pin!(refresh);
        let mut timed_out = false;
        let result = tokio::select! {
            result = &mut refresh => result,
            _ = tokio::time::sleep(Duration::from_millis(15_000)) => {
                timed_out = true;
                signal.cancel();
                let _ = refresh.await;
                Err(kiana_ai::error::AiError::aborted("This operation was aborted"))
            }
        };
        match result {
            Ok(refresh_result) => {
                if refresh_result.aborted && timed_out {
                    self.show_warning("Model refresh timed out; searching cached models.");
                } else if !refresh_result.errors.is_empty() {
                    let keys: Vec<String> = refresh_result.errors.keys().cloned().collect();
                    self.show_warning(&format!(
                        "Could not refresh {}; searching cached models.",
                        keys.join(", ")
                    ));
                }
            }
            Err(error) => {
                if timed_out {
                    self.show_warning("Model refresh timed out; searching cached models.");
                } else {
                    self.show_warning(&format!("Could not refresh model catalogs: {error}"));
                }
            }
        }
        find_exact_model_reference_match(
            search_term,
            &session.model_runtime().get_available_snapshot(),
        )
    }

    /// `showModelSelector(initialSearchInput?)`。
    ///
    /// 把 [`ModelSelectorComponent`] 挂到编辑器槽位（经由 [`super::show_selector`]）。
    /// 本函数只负责在闭包外读快照（当前模型、scoped 列表、settings 里的默认模型），把
    /// 这些不可变初值 clone 进构造闭包；组件内的交互（搜索、启用列表刷新）由组件自己
    /// 通过 `model_runtime` 句柄完成。
    ///
    /// 回调生命周期：`on_select` / `on_select_as_default` / `on_cancel` 都只捕获
    /// `Weak`，模式释放后升级失败即静默跳过。`on_select_as_default` 与 `on_select`
    /// 的唯一差别是 `persist`——后者决定 `set_model` 是否写入 settings 默认值。
    pub(crate) fn show_model_selector(&mut self, initial_search_input: Option<String>) {
        let session = self.session();
        let current_model = session.model();
        let model_runtime = session.model_runtime().clone();
        let scoped_models: Vec<ScopedModelItem> = session
            .scoped_models()
            .into_iter()
            .map(|scoped| ScopedModelItem {
                model: scoped.model,
                thinking_level: scoped.thinking_level.map(super::level_str),
            })
            .collect();
        let settings_manager = self.settings_manager();
        let default_provider = settings_manager.get_default_provider();
        let default_model_id = settings_manager.get_default_model();
        let default_model = match (default_provider, default_model_id) {
            (Some(provider), Some(id)) if !provider.is_empty() && !id.is_empty() => {
                Some(DefaultModelReference { provider, id })
            }
            _ => None,
        };
        let request_render = Some(self.request_render_callback());
        self.show_selector(move |mode, done| {
            let weak = mode.downgrade();
            let select_model = {
                let weak = weak.clone();
                let done = done.clone();
                move |model: Model, persist: bool| {
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    let mut mode = InteractiveMode { inner };
                    let done = done.clone();
                    tokio::task::spawn_local(async move {
                        mode.select_model_from_selector(model, persist, done).await;
                    });
                }
            };
            let on_select = {
                let select_model = select_model.clone();
                Box::new(move |model: Model| select_model(model, false))
            };
            let on_select_as_default: Box<dyn FnMut(Model)> = {
                let select_model = select_model;
                Box::new(move |model: Model| select_model(model, true))
            };
            let weak_cancel = weak.clone();
            let done_cancel = done.clone();
            let on_cancel = Box::new(move || {
                done_cancel.finish();
                if let Some(inner) = weak_cancel.upgrade() {
                    InteractiveMode { inner }.request_render();
                }
            });
            let selector = ModelSelectorComponent::new(
                request_render.clone(),
                current_model.clone(),
                model_runtime.clone(),
                scoped_models.clone(),
                on_select,
                on_cancel,
                initial_search_input.clone(),
                Some(on_select_as_default),
                default_model.clone(),
            );
            let handle = Rc::new(RefCell::new(selector));
            let dispose_handle = Rc::clone(&handle);
            SelectorCreation {
                component: Rc::clone(&handle) as kiana_tui::layout_node::SharedComponent,
                focus: SelectorFocus::Component,
                dispose: Some(Box::new(move || {
                    if let Ok(mut selector) = dispose_handle.try_borrow_mut() {
                        selector.dispose();
                    }
                })),
            }
        });
    }

    /// `selectModel(model, persist)`（`showModelSelector` 的选中回调）。
    ///
    /// 成功路径的顺序是冻结契约：先 `done.finish()` 关闭选择器，再显示状态行、再请求
    /// Anthropic 订阅警告检查（可能追加第二条状态）。失败路径同样要先 `finish()`——
    /// 选择器不能因为切模型失败而留在屏幕上。
    ///
    /// `persist` 为真时 `set_model` 会把该模型写入 settings 的默认 provider/model
    /// （见 `kiana-session` 的 `set_model_impl`），状态行因此显示 `Default model: …`。
    async fn select_model_from_selector(
        &mut self,
        model: Model,
        persist: bool,
        done: super::SelectorDone,
    ) {
        let result = self
            .session()
            .set_model(
                model.clone(),
                kiana_session::agent_session::ModelMutationOptions {
                    persist: Some(persist),
                },
            )
            .await;
        match result {
            Ok(()) => {
                self.update_available_provider_count();
                self.inner.borrow().footer.borrow_mut().invalidate();
                self.update_editor_border_color();
                // TS：`done()` 在 showStatus 之前关闭选择器（成功与失败分支都会调用）。
                done.finish();
                self.show_status(&if persist {
                    format!("Default model: {}/{}", model.provider, model.id)
                } else {
                    format!("Model: {}", model.id)
                });
                self.maybe_warn_about_anthropic_subscription_auth_for(Some(model))
                    .await;
            }
            Err(error) => {
                done.finish();
                self.show_error(&error.to_string());
            }
        }
    }

    /// `showModelsSelector()`。
    ///
    /// `/scoped-models` 命令：打开多选模型选择器，圈定本会话可用的模型集合。
    ///
    /// # 共享状态与回调
    ///
    /// 函数体在构造选择器前先建好 4 份 `Rc` 共享状态，因为它们要同时被「组件回调」、
    /// 「刷新续体」和「会话写回」三方读写：
    ///
    /// - `available_models` / `available_model_ids`：当前可用模型快照及其 `model_key`
    ///   集合；刷新续体完成后原位替换。
    /// - `current_enabled_ids`：当前启用集合。会话已有 scoped models 时以会话为准；
    ///   否则用 settings 里的 `enabledModels` 模式解析（`configured_enabled_ids`）。
    /// - `selection_changed`：用户是否动过选择。刷新完成时若用户已动过（或会话原本
    ///   就有 scoped models），用 `update_models_keep_enabled` 保留用户选择，避免
    ///   后台刷新把用户的即时操作覆盖掉。
    ///
    /// 回调语义：
    ///
    /// - `on_change`：每次选择变化即时写回会话的 scoped models（`update_session_models`）。
    ///   写回规则：启用集合为 `None`、或它一个可用模型都没圈到、或它圈到了全部可用模型
    ///   时，清空 scoped（等于不圈）；否则按集合解析写入。每次变化后都更新 footer 的
    ///   可见 provider 数并请求重绘。
    /// - `on_persist`：把启用集合写入 settings 的 `enabledModels`。注意「全选」与
    ///   「未选择（`None`）」都持久化为 `None`（去掉配置项），只有真正的子集才写成
    ///   模式列表。
    /// - `on_cancel`：只关闭选择器，不回滚会话里已经生效的 scoped 变更。
    ///
    /// # 后台目录刷新
    ///
    /// 选择器挂载后立即 `spawn_local` 一次 15 秒超时的目录刷新；续体完成后把新快照
    /// 推进共享状态、选择器与会话（经 `update_session_models`）。续体第一件事检查
    /// `disposed`：选择器已被关闭（挂载了新选择器，见 `tests/interactive_mode_model_auth.rs`
    /// 的 review 测试）则直接返回，不得再改会话状态或请求重绘。
    pub(crate) fn show_models_selector(&mut self) {
        let session = self.session();
        let settings_manager = self.settings_manager();
        let configured_patterns = settings_manager.get_enabled_models();
        let session_scoped_models = session.scoped_models();
        let initial_models = session.model_runtime().get_available_snapshot();

        let available_models: Rc<RefCell<Vec<Model>>> =
            Rc::new(RefCell::new(initial_models.clone()));
        let available_model_ids: Rc<RefCell<HashSet<String>>> =
            Rc::new(RefCell::new(initial_models.iter().map(model_key).collect()));

        let configured_enabled_ids = {
            // 把 settings 里的 `enabledModels` 模式解析成模型键列表。返回 `None` 的两种
            // 情形：settings 没配置该项、或配置为空列表——语义上等价于「未圈选」。
            // 解析诊断里 `NoMatch` 的模式会被追加进结果（保持插入序），让用户在选择器里
            // 看到配置里存在但当前目录匹配不到的模式。
            let configured_patterns = configured_patterns.clone();
            move |models: &[Model]| -> Option<Vec<String>> {
                let patterns = configured_patterns.as_ref()?;
                if patterns.is_empty() {
                    return None;
                }
                let resolved = resolve_model_scope_from_models(patterns, models);
                let mut ids: Vec<String> = resolved
                    .scoped_models
                    .iter()
                    .map(|scoped| model_key(&scoped.model))
                    .collect();
                for diagnostic in &resolved.diagnostics {
                    if diagnostic.code
                        == kiana_config::models::resolver::ModelScopeDiagnosticCode::NoMatch
                        && !ids.contains(&diagnostic.pattern)
                    {
                        ids.push(diagnostic.pattern.clone());
                    }
                }
                Some(ids)
            }
        };

        let current_enabled_ids: Rc<RefCell<Option<Vec<String>>>> =
            Rc::new(RefCell::new(if session_scoped_models.is_empty() {
                configured_enabled_ids(&available_models.borrow())
            } else {
                Some(
                    session_scoped_models
                        .iter()
                        .map(|scoped| model_key(&scoped.model))
                        .collect(),
                )
            }));
        let selection_changed = Rc::new(Cell::new(false));

        let update_session_models = {
            // 把一组启用 id 写回会话的 scoped models，随后刷新 footer 计数并请求重绘。
            // 由 `on_change`（用户即时操作）与刷新续体共用。判定条件与 TS
            // `updateSessionModels` 一致：只有「非空集合且至少圈到一个可用模型且没有
            // 圈满全部可用模型」才真正设置 scoped，其余（全选/清空/悬空）一律清空 scoped。
            let session = session.clone();
            let available_models = Rc::clone(&available_models);
            let available_model_ids = Rc::clone(&available_model_ids);
            let current_enabled_ids = Rc::clone(&current_enabled_ids);
            let weak = self.downgrade();
            Rc::new(move |enabled_ids: Option<Vec<String>>| {
                *current_enabled_ids.borrow_mut() = enabled_ids.clone();
                let ids = available_model_ids.borrow();
                let has_enabled_available_model = enabled_ids
                    .as_ref()
                    .is_some_and(|enabled| enabled.iter().any(|id| ids.contains(id)));
                let all_available_models_enabled = enabled_ids
                    .as_ref()
                    .is_some_and(|enabled| ids.iter().all(|id| enabled.contains(id)));
                let models = available_models.borrow();
                if let Some(enabled_ids) = &enabled_ids
                    && has_enabled_available_model
                    && !all_available_models_enabled
                {
                    let resolved = resolve_model_scope_from_models(enabled_ids, &models);
                    session.set_scoped_models(
                        resolved
                            .scoped_models
                            .into_iter()
                            .map(|scoped| kiana_session::agent_session::ScopedModel {
                                model: scoped.model,
                                thinking_level: scoped.thinking_level,
                            })
                            .collect(),
                    );
                } else {
                    session.set_scoped_models(Vec::new());
                }
                if let Some(inner) = weak.upgrade() {
                    let mut mode = InteractiveMode { inner };
                    mode.update_available_provider_count();
                    mode.request_render();
                }
            })
        };

        let settings_for_persist = self.settings_manager();
        let available_models_for_config = Rc::clone(&available_models);
        let available_model_ids_for_persist = Rc::clone(&available_model_ids);
        let weak_cancel = self.downgrade();
        let weak_status = self.downgrade();
        let update_for_change = Rc::clone(&update_session_models);
        let update_for_refresh_source = Rc::clone(&update_session_models);
        let selection_changed_for_change = Rc::clone(&selection_changed);

        self.show_selector(move |mode, done| {
            let selector_slot: Rc<RefCell<Option<Rc<RefCell<ScopedModelsSelectorComponent>>>>> =
                Rc::new(RefCell::new(None));
            let callbacks = ModelsCallbacks {
                on_change: Box::new(move |enabled_ids: Option<Vec<String>>| {
                    selection_changed_for_change.set(true);
                    update_for_change(enabled_ids);
                    Box::pin(async {}) as ModelsCallbackFuture
                }),
                on_persist: Box::new(move |enabled_ids: Option<Vec<String>>| {
                    let models = available_models_for_config.borrow();
                    let ids = available_model_ids_for_persist.borrow();
                    let all_enabled = enabled_ids.as_ref().is_some_and(|enabled| {
                        enabled.len() == models.len() && enabled.iter().all(|id| ids.contains(id))
                    });
                    let new_patterns = match &enabled_ids {
                        None => None,
                        Some(_) if all_enabled => None,
                        Some(enabled) => Some(enabled.clone()),
                    };
                    settings_for_persist.set_enabled_models(new_patterns);
                    if let Some(inner) = weak_status.upgrade() {
                        InteractiveMode { inner }.show_status("Model selection saved to settings");
                    }
                    Box::pin(async {}) as ModelsCallbackFuture
                }),
                on_cancel: {
                    let done = done.clone();
                    let weak = weak_cancel.clone();
                    Box::new(move || {
                        done.finish();
                        if let Some(inner) = weak.upgrade() {
                            InteractiveMode { inner }.request_render();
                        }
                    })
                },
            };
            let config = ModelsConfig {
                all_models: available_models.borrow().clone(),
                enabled_model_ids: current_enabled_ids.borrow().clone(),
                refresh_status: Some("Refreshing model catalogs…".to_string()),
            };
            let selector = ScopedModelsSelectorComponent::new(config, callbacks);
            let shared = Rc::new(RefCell::new(selector));
            *selector_slot.borrow_mut() = Some(Rc::clone(&shared));

            // refreshModelCatalogs（15s 超时）在后台更新选择器。
            //
            // 竞速结构：共享刷新 token 与 15 秒定时器 `select!`。超时侧 cancel token 后
            // 仍 `refresh.await` 等底层任务收尾（共享协调器要求等待者拿走结果），再构造
            // aborted 错误走警告分支。整个续体是一次性的：若等待期间选择器被 dispose，
            // `disposed` 检查让后续所有写共享状态/写会话的步骤都不执行。
            let disposed = Rc::new(Cell::new(false));
            let disposed_for_refresh = Rc::clone(&disposed);
            let signal = CancellationToken::new();
            let shared_for_refresh = Rc::clone(&shared);
            let available_models_for_refresh = Rc::clone(&available_models);
            let available_ids_for_refresh = Rc::clone(&available_model_ids);
            let current_ids_for_refresh = Rc::clone(&current_enabled_ids);
            let update_for_refresh = Rc::clone(&update_for_refresh_source);
            let configured_for_refresh = configured_enabled_ids.clone();
            let session_scoped_len = session_scoped_models.len();
            let selection_changed_for_refresh = Rc::clone(&selection_changed);
            let weak_refresh = mode.downgrade();
            let session_refresh = session.clone();
            let refresh_signal = signal.clone();
            tokio::task::spawn_local(async move {
                let runtime = session_refresh.model_runtime().clone();
                let refresh = refresh_model_catalogs(&runtime, Some(&refresh_signal));
                tokio::pin!(refresh);
                let mut timed_out = false;
                let result = tokio::select! {
                    result = &mut refresh => result,
                    _ = tokio::time::sleep(Duration::from_millis(15_000)) => {
                        timed_out = true;
                        refresh_signal.cancel();
                        let _ = refresh.await;
                        Err(kiana_ai::error::AiError::aborted("This operation was aborted"))
                    }
                };
                if disposed_for_refresh.get() {
                    return;
                }
                let new_models = session_refresh.model_runtime().get_available_snapshot();
                // 刷新后推进选择器：用户已动过选择或会话本就有 scoped models 时只换
                // 模型清单、保留当前启用集合；否则按 settings 的 enabledModels 重新解析
                // 出启用集合（config 模式可能在新目录下圈到不同模型）。
                let new_ids: HashSet<String> = new_models.iter().map(model_key).collect();
                *available_models_for_refresh.borrow_mut() = new_models.clone();
                *available_ids_for_refresh.borrow_mut() = new_ids;
                {
                    let mut selector = shared_for_refresh.borrow_mut();
                    if !selection_changed_for_refresh.get() && session_scoped_len == 0 {
                        let configured = configured_for_refresh(&new_models);
                        *current_ids_for_refresh.borrow_mut() = configured.clone();
                        selector.update_models_with_enabled(&new_models, configured);
                    } else {
                        selector.update_models_keep_enabled(&new_models);
                    }
                }
                let enabled = current_ids_for_refresh.borrow().clone();
                // 启用集合非 `None` 时，刷新出的新清单同样要写回会话（与 `on_change`
                // 同路径），否则新出现的已启用模型不会生效。
                if enabled.is_some() {
                    update_for_refresh(enabled);
                }
                let (message, kind) = match result {
                    // 三种用户可见结局：超时降级、部分 provider 失败降级、完全成功。
                    // 与 `/model` 的刷新分支共用措辞（"…; searching cached models." 对应
                    // 这里的 "…; showing cached models."），错误键取 provider id 的插入序。
                    Ok(refresh_result) => {
                        if refresh_result.aborted && timed_out {
                            (
                                "Model refresh timed out; showing cached models.".to_string(),
                                RefreshStatusKind::Warning,
                            )
                        } else if !refresh_result.errors.is_empty() {
                            let keys: Vec<String> = refresh_result.errors.keys().cloned().collect();
                            (
                                format!(
                                    "Could not refresh {}; showing cached models.",
                                    keys.join(", ")
                                ),
                                RefreshStatusKind::Warning,
                            )
                        } else {
                            (
                                "Model catalogs refreshed.".to_string(),
                                RefreshStatusKind::Success,
                            )
                        }
                    }
                    Err(error) => (
                        if timed_out {
                            "Model refresh timed out; showing cached models.".to_string()
                        } else {
                            format!("Could not refresh model catalogs: {error}")
                        },
                        RefreshStatusKind::Warning,
                    ),
                };
                shared_for_refresh
                    .borrow_mut()
                    .set_refresh_status(&message, kind);
                if let Some(inner) = weak_refresh.upgrade() {
                    InteractiveMode { inner }.request_render();
                }
            });

            let dispose_signal = signal;
            let disposed_for_dispose = Rc::clone(&disposed);
            SelectorCreation {
                component: Rc::clone(&shared) as kiana_tui::layout_node::SharedComponent,
                focus: SelectorFocus::Component,
                dispose: Some(Box::new(move || {
                    // TS: `dispose: () => { disposed = true; clearTimeout(timeout); controller.abort(); }`
                    // ——刷新续体在 `if (disposed) return;` 处短路。
                    // Rust 版没有 `setTimeout`：定时器在续体内部的 `select!` 里，cancel
                    // signal 即可同时中断刷新等待与定时器分支。
                    disposed_for_dispose.set(true);
                    dispose_signal.cancel();
                })),
            }
        });
    }

    /// `showModelsSelector()` 的测试接缝。
    #[doc(hidden)]
    pub fn test_show_models_selector(&mut self) {
        self.show_models_selector();
    }

    /// `handleModelCommand` 的测试接缝。
    #[doc(hidden)]
    pub async fn test_handle_model_command(&mut self, search_term: Option<String>) {
        self.handle_model_command(search_term).await;
    }

    /// `findExactModelMatch` 的测试接缝。
    #[doc(hidden)]
    pub async fn test_find_exact_model_match(&mut self, search_term: &str) -> Option<Model> {
        self.find_exact_model_match(search_term).await
    }

    /// `showModelSelector` 的测试接缝。
    #[doc(hidden)]
    pub fn test_show_model_selector(&mut self, initial_search_input: Option<String>) {
        self.show_model_selector(initial_search_input);
    }

    /// 当前已配置的 scoped 模型（测试用）。
    #[doc(hidden)]
    pub fn test_scoped_model_keys(&self) -> Vec<String> {
        self.session()
            .scoped_models()
            .iter()
            .map(|scoped| format!("{}/{}", scoped.model.provider, scoped.model.id))
            .collect()
    }

    /// `modelRuntime` 的快照（测试用）。
    #[doc(hidden)]
    pub fn test_model_runtime(&self) -> ModelRuntime {
        self.session().model_runtime().clone()
    }

    /// 可见的 provider 数（测试用）。
    #[doc(hidden)]
    pub fn test_available_provider_count(&self) -> usize {
        self.inner
            .borrow()
            .footer_data_provider
            .get_available_provider_count()
    }
}
