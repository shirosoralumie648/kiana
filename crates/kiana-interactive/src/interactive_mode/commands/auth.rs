//! `interactive-mode.ts` 的 `/trust`、`/login`、`/logout`（§2.50.12）。
//!
//! # 职责与上游
//!
//! 三条斜杠命令的交互流程：`/trust` 的选择器与 `trust.json` 写入；`/login` 的“认证方式 →
//! provider → 登录对话框 → 选择默认模型”链路；`/logout` 的凭据删除。命令分派在
//! `crate::interactive_mode::commands` 的私有函数 `dispatch_submit` 里（`/trust`、
//! `/login`、`/logout` 三个分支）；Anthropic 订阅用量警告（`maybe_warn_*`）还被模型切换
//! 路径（`commands/model.rs`、`commands/mod.rs`）与启动路径（`interactive_mode.rs`）调用。
//!
//! # 为什么需要 UI 桥
//!
//! 真正的登录由 `kiana-config` 的 `ModelRuntime::login` / `logout` 与 `kiana-ai` 的 auth
//! 实现完成；它们需要向用户要授权码、展示设备码与进度，但本 crate 的 UI 组件由
//! `Rc<RefCell<…>>` 持有，不能跨线程。于是：
//!
//! 1. `AuthUiBridge` 实现 `kiana_ai::auth::types::AuthInteraction`（要求 `Send + Sync`），
//!    把 `prompt` / `notify` 转成 `AuthUiMessage`，经无界通道发回 UI 线程；
//! 2. `spawn_auth_ui_loop` 在本地任务（`spawn_local`）里消费消息、驱动
//!    `LoginDialogComponent`；
//! 3. 每个 `prompt` 附带一个 oneshot，登录逻辑像普通 `await` 一样等待用户输入。
//!
//! 通道关闭（UI 循环退出）或输入被取消时，`prompt` 返回
//! `AuthError { message: "Login cancelled" }`，登录流程据此走“用户取消”分支。整个流程的
//! 取消信号来自对话框的 `CancellationToken`：`login_provider` 用 `AbortScope::from(...)`
//! 包装后交给 runtime，用户按 Esc 时两侧共享同一取消状态。
//!
//! # 一次 `/login` 的执行顺序
//!
//! 1. `handle_login_command`：无参数 → 认证方式选择器；有参数时按 id/显示名精确匹配；
//! 2. `show_login_auth_type_selector` / `show_login_provider_selector`：选择 OAuth 或 API
//!    key，以及具体 provider；
//! 3. `start_provider_login` → `show_login_dialog`（OAuth）或 `show_api_key_login_dialog`
//!    （API key）；`ApiKeyAuth::has_login()` 为 `false` 的 provider 走
//!    `show_ambient_auth_dialog`（只提示“在外部配置”）；
//! 4. `login_provider` 挂对话框、起 UI 循环，然后 await `ModelRuntime::login`；
//! 5. `complete_provider_authentication`：按需切换默认模型，并在后台刷新该 provider 的
//!    模型目录（不阻塞登录）；Radius 的 OAuth 登录还会追加
//!    `offer_radius_mcp_server`。
//!
//! # 本文件的结构（阅读顺序）
//!
//! - 线程边界：`AuthUiMessage` / `AuthUiBridge` 把 `AuthInteraction` 的调用转成跨线程消息；
//!   文末的 `spawn_auth_ui_loop`、`notify_auth_dialog`、`show_auth_prompt_ui`、
//!   `show_auth_select` 在 UI 线程消费消息并驱动 `LoginDialogComponent`。
//! - `/trust`：`show_trust_selector`。
//! - `/login`：`handle_login_command` → 认证方式/provider 选择器 → `start_provider_login` →
//!   登录对话框 → `login_provider` → `complete_provider_authentication` /
//!   `finish_authentication`。
//! - `/logout`：`show_oauth_selector` → `logout_provider`。
//! - 登录后的附加流程：`offer_radius_mcp_server`（Radius MCP 配置）与
//!   `maybe_warn_about_anthropic_subscription_auth*`（订阅用量警告）。
//! - 纯函数与测试接缝：`login_provider_options`、`get_auth_credential`、`test_*`。
//!
//! # 约束与失败模式
//!
//! - **必须在本地任务中运行**：本文件大量使用 `tokio::task::spawn_local`，调用链来自交互
//!   模式的单线程 `LocalSet`；`spawn_local` 要求当前线程有 `LocalSet`。
//! - **后台任务与选择器回调持 `Weak`**：它们可能晚于界面变更，升级失败就放弃；登录后的
//!   刷新分支还额外比较 `session_id` 与当前模型，避免覆盖用户的新选择。
//! - **超时**：清点凭据、登出、登录后的目录刷新各有一个 15 秒定时器。目录刷新超时只显示
//!   警告并使用缓存模型，登录本身已经成功（测试
//!   `interactive_login_completes_before_its_bounded_background_refresh` 锁定）。
//! - **文案是契约**：`show_status` / `show_error` 的字符串是当前稳定契约，集成测试直接
//!   断言（`tests/interactive_mode_model_auth.rs`），修改需同步测试。
//! - **测试接缝**：文末的 `#[doc(hidden)] pub fn test_*` 只是让 crate 外的集成测试能驱动
//!   私有流程，不属于公开 API。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use kiana_ai::auth::oauth_compat::OAuthDeviceCodeInfo;
use kiana_ai::auth::types::{
    AuthCheck, AuthEvent, AuthInteraction, AuthOperationOptions, AuthPrompt, AuthPromptRequest,
    AuthType, LoginOptions,
};
use kiana_ai::utils::abort::AbortScope;
use kiana_config::models::resolver::DEFAULT_MODEL_PER_PROVIDER;
use kiana_config::models::runtime::{ModelRuntime, ModelRuntimeError};
use kiana_config::trust::manager::ProjectTrustStore;
use kiana_json::js_trim;
use kiana_tui::tui::Container;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

use crate::components::extension_selector::{ExtensionSelectorComponent, ExtensionSelectorOptions};
use crate::components::login_dialog::LoginDialogComponent;
use crate::components::oauth_selector::{
    AuthSelectorMethod, AuthSelectorMode, AuthSelectorProvider, OAuthSelectorComponent,
    format_auth_selector_provider_status,
};
use crate::components::radius_login_selector::{RadiusOption, create_login_menu_selector};
use crate::components::trust_selector::{TrustSelectorComponent, TrustSelectorOptions};
use crate::interactive_mode::ANTHROPIC_SUBSCRIPTION_AUTH_WARNING;
use crate::interactive_mode::is_anthropic_subscription_auth_key;
use crate::tui_renderer::{InteractiveTuiReference, SharedComponentProxy, component_address};

use super::super::InteractiveMode;
use super::{SelectorCreation, SelectorFocus, shared_component};

/// 用主题色给文本着色（`kiana_theme::THEME.fg` 的薄包装）。
///
/// 主题未初始化时 `fg` 返回 `Err`；这里按调用点的 `expect` 直接 panic（错误文案注明
/// TS 也在同一位置抛错），因此调用方必须保证渲染前主题已经就绪。
fn fg(color: kiana_theme::theme_tokens::ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// `handleLoginCommand` 的 `onBack`：取消后重新打开上一层选择器。
///
/// 两条路径会用到它：OAuth/API key 流程在 `login_provider` 返回 `Login cancelled` 时调用；
/// ambient 流程由登录对话框的 `on_complete`（只在用户按取消键关闭对话框时触发，取消键是
/// `tui.select.cancel`，默认绑定为 Esc / Ctrl+C）调用。回调拿到当前 `InteractiveMode`，
/// 因此可以同步调用上一层的 `show_login_*_selector` 把选择器重新
/// 挂回编辑器槽位。闭包会被移进 `spawn_local` 的任务，故必须是 `'static`。
type OnBack = Box<dyn FnMut(&mut InteractiveMode)>;

/// 登录 UI 请求（从 `AuthInteraction` 桥接到 UI 线程）。
///
/// `Prompt` 携带应答端：`AuthUiBridge::prompt` 在发送后 await 接收端，因此登录逻辑不需要
/// 知道 UI 线程的存在；`Notify` 是单向事件，发送失败直接丢弃。通道是无界的，`notify`
/// 与 `prompt` 都不会阻塞调用方。
enum AuthUiMessage {
    /// 显示提示并等待输入。
    Prompt {
        /// 提示。
        request: AuthPromptRequest,
        /// 应答端：UI 循环把用户输入或取消错误发回给正在 await 的 `prompt`；若 UI 循环
        /// 先退出、发送端被丢弃，`prompt` 的接收会失败并按取消处理。
        respond: tokio::sync::oneshot::Sender<Result<String, kiana_ai::auth::types::AuthError>>,
    },
    /// 通知事件。
    Notify(AuthEvent),
}

/// `AuthInteraction` 的 UI 桥（必须 `Send + Sync`；真正的组件在 UI 线程上被驱动）。
///
/// `signal` 是对话框 `CancellationToken` 的包装（`login_provider` 里用
/// `AbortScope::from(dialog.borrow().signal())` 构造），与对话框共享同一取消状态：对话框
/// 取消后 runtime 侧的登录流程也会看到 signal 已中止。`sender` 是无界通道，`notify` 永不
/// 阻塞；UI 循环退出后发送失败，`notify` 忽略、`prompt` 转成 `"Login cancelled"`。
struct AuthUiBridge {
    signal: AbortScope,
    sender: tokio::sync::mpsc::UnboundedSender<AuthUiMessage>,
}

#[async_trait::async_trait]
impl AuthInteraction for AuthUiBridge {
    fn signal(&self) -> &AbortScope {
        &self.signal
    }

    async fn prompt(
        &self,
        request: AuthPromptRequest,
    ) -> Result<String, kiana_ai::auth::types::AuthError> {
        // 无界通道：发送不会阻塞登录所在的线程；失败说明 UI 循环已经退出。
        let (respond, receiver) = tokio::sync::oneshot::channel();
        if self
            .sender
            .send(AuthUiMessage::Prompt { request, respond })
            .is_err()
        {
            return Err(kiana_ai::auth::types::AuthError::new("Login cancelled"));
        }
        // 应答端被丢弃（UI 循环在发送结果前结束）同样按“用户取消”处理：prompt 必须
        // 有确定的返回值，不能让登录逻辑永久挂起。
        receiver
            .await
            .unwrap_or_else(|_| Err(kiana_ai::auth::types::AuthError::new("Login cancelled")))
    }

    fn notify(&self, event: &AuthEvent) {
        // 事件是单向的：UI 已退出时丢弃即可，通知失败绝不能影响登录流程。
        let _ = self.sender.send(AuthUiMessage::Notify(event.clone()));
    }
}

impl InteractiveMode {
    /// `maybeWarnAboutAnthropicSubscriptionAuth(model?)`。
    ///
    /// 无参版本：用会话当前模型判断；交互模式启动时（`InteractiveMode::run`）与登录收尾在
    /// “没有刚选中的模型”时用这个入口。
    pub(crate) async fn maybe_warn_about_anthropic_subscription_auth(&mut self) {
        self.maybe_warn_about_anthropic_subscription_auth_for(None)
            .await;
    }

    /// `maybeWarnAboutAnthropicSubscriptionAuth(model?)`。
    ///
    /// 当 Anthropic 正在使用订阅凭证时提示一次用量警告（第三方 harness 的订阅用量按
    /// token 另行计费）。判定顺序：
    ///
    /// 1. 设置里 `warnings.anthropic_extra_usage` 为 `Some(false)` 时完全关闭；
    /// 2. 本会话已经提示过（`anthropic_subscription_warning_shown`）则跳过；
    /// 3. 模型为空或 provider 不是 `"anthropic"` 则跳过；
    /// 4. `check_auth("anthropic")` 返回 OAuth → 提示；查询出错则与 TS 的
    ///    `try { … } catch { /* ignore */ }` 一样直接返回（不会再查 `getAuth`）；
    /// 5. 否则 `get_auth_for_provider` 只读 `apiKey`，要求 `is_anthropic_subscription_auth_key`
    ///    （前缀 `sk-ant-oat`）为真；`None` 或空串都不提示。
    ///
    /// “只提示一次”由本方法在真正 `show_warning` 前写标志保证；后续切换模型不会再打扰。
    pub(crate) async fn maybe_warn_about_anthropic_subscription_auth_for(
        &mut self,
        model: Option<kiana_ai::types::Model>,
    ) {
        let settings_manager = self.settings_manager();
        if settings_manager.get_warnings().anthropic_extra_usage == Some(false) {
            return;
        }
        if self.inner.borrow().anthropic_subscription_warning_shown {
            return;
        }
        let model = model.or_else(|| self.session().model());
        let Some(model) = model else {
            return;
        };
        if model.provider != "anthropic" {
            return;
        }

        let runtime = self.session().model_runtime().clone();
        let check = runtime
            .check_auth("anthropic", AuthOperationOptions::default())
            .await;
        let Ok(check) = check else {
            // 与 TS 的 `try { … } catch { /* ignore */ }` 一致：查询失败只忽略。
            return;
        };
        if let Some(check) = check
            && check.r#type == AuthType::OAuth
        {
            self.inner.borrow_mut().anthropic_subscription_warning_shown = true;
            self.show_warning(ANTHROPIC_SUBSCRIPTION_AUTH_WARNING);
            return;
        }
        let auth = runtime
            .get_auth_for_provider("anthropic", Default::default())
            .await;
        let Ok(Some(auth)) = auth else {
            return;
        };
        // TS: `(await modelRuntime.getAuth(provider))?.auth.apiKey`。只读 `apiKey`：
        // `ANTHROPIC_AUTH_TOKEN` 只解析成 `Authorization: Bearer …` 头（`apiKey` 为
        // `undefined`），TS 不据此告警，这里也不能改用 `getAuthCredential` 的头回退。
        if !is_anthropic_subscription_auth_key(auth.auth.api_key.as_deref()) {
            return;
        }
        self.inner.borrow_mut().anthropic_subscription_warning_shown = true;
        self.show_warning(ANTHROPIC_SUBSCRIPTION_AUTH_WARNING);
    }

    /// `showTrustSelector()`。
    ///
    /// 读取 `trust.json` 里的最近决定与当前会话的信任状态（设置管理器），交给
    /// `TrustSelectorComponent`；选中后把 `selection.updates` 写回信任库，并提示
    /// “重启 <APP_NAME> 后生效”。取消只重绘，不写文件。
    ///
    /// 写回的是一个 `ProjectTrustUpdate` 列表（可能同时包含当前目录与父目录的删除/设置），
    /// `set_many` 的 `Result` 被丢弃：写盘失败时界面仍显示 “Saved trust decision”。
    pub(crate) fn show_trust_selector(&mut self) {
        let cwd = self.session_manager().get_cwd().to_string();
        let agent_dir = self
            .inner
            .borrow()
            .runtime_host
            .services()
            .agent_dir
            .clone();
        let trust_store = ProjectTrustStore::new(&agent_dir);
        // 读取/解析失败（例如 trust.json 损坏）被当作“没有已保存决定”；这只是展示信息，
        // 不参与本次会话的信任判定。
        let saved_decision = trust_store.get_entry(&cwd).ok().flatten();
        let project_trusted = self.settings_manager().is_project_trusted();
        self.show_selector(move |mode, done| {
            let weak = mode.downgrade();
            let select_done = done.clone();
            let store = ProjectTrustStore::new(&agent_dir);
            let on_select = Box::new(
                move |selection: crate::components::trust_selector::TrustSelection| {
                    // 写盘错误被丢弃：选择流程与“已保存”提示都不受影响（结果可能未落盘）。
                    let _ = store.set_many(&selection.updates);
                    // done() 关闭选择器并把编辑器槽位的恢复排到下一个本地任务；之后升级
                    // 失败说明界面已销毁，不再提示。
                    select_done.finish();
                    if let Some(inner) = weak.upgrade() {
                        let mut mode = InteractiveMode { inner };
                        mode.show_status(&format!(
                            "Saved trust decision: {}. Restart {} for this to take effect.",
                            if selection.trusted {
                                "trusted"
                            } else {
                                "untrusted"
                            },
                            kiana_core::config::APP_NAME
                        ));
                    }
                },
            );
            let weak_cancel = mode.downgrade();
            let on_cancel = Box::new(move || {
                done.finish();
                if let Some(inner) = weak_cancel.upgrade() {
                    InteractiveMode { inner }.request_render();
                }
            });
            let selector = TrustSelectorComponent::new(TrustSelectorOptions {
                cwd: cwd.clone(),
                saved_decision: saved_decision.clone(),
                project_trusted,
                on_select,
                on_cancel,
            });
            SelectorCreation {
                component: shared_component(selector),
                focus: SelectorFocus::Component,
                dispose: None,
            }
        });
    }

    /// `getLoginProviderOptions(authType?)`。
    ///
    /// 只是转发到文件底部的纯函数 `login_provider_options`；`&self` 版本供 UI 直接调用，
    /// 纯函数版本给自动补全的 `Send + Sync` 闭包使用。
    pub(crate) fn get_login_provider_options(
        &self,
        auth_type: Option<AuthType>,
    ) -> Vec<AuthSelectorProvider> {
        login_provider_options(&self.session().model_runtime().clone(), auth_type)
    }

    /// `getLogoutProviderOptions()`。
    ///
    /// 列出已保存的凭据（`list_credentials`），投影成选择器条目：名称缺失时回退为
    /// provider id，`status.source` 固定为 `"stored credential"`（选择器据此把条目显示为
    /// 已配置），最后按显示名做 `localeCompare` 排序。
    ///
    /// 15 秒超时用本地 `AbortScope` 模拟 TS 的 `AbortSignal.timeout(15_000)`：定时任务到点
    /// 以 `TimeoutError` 中止信号；`list_credentials` 的错误转成 `error.message` 返回，
    /// 调用方把它显示为 “Could not read stored credentials: …”。
    async fn get_logout_provider_options(&self) -> Result<Vec<AuthSelectorProvider>, String> {
        let runtime = self.session().model_runtime().clone();
        // TS：`listCredentials({ signal: AbortSignal.timeout(15_000) })`。
        let signal = AbortScope::new();
        let timeout_signal = signal.clone();
        tokio::task::spawn_local(async move {
            tokio::time::sleep(Duration::from_millis(15_000)).await;
            timeout_signal.abort_with(kiana_ai::utils::abort::AbortReason {
                name: "TimeoutError".to_string(),
                message: "The operation was aborted due to timeout".to_string(),
            });
        });
        let credentials = runtime
            .list_credentials(AuthOperationOptions {
                signal: Some(signal),
            })
            .await
            .map_err(|error| error.message.clone())?;
        let mut options: Vec<AuthSelectorProvider> = credentials
            .into_iter()
            .map(|credential| {
                let provider = runtime.get_provider(&credential.provider_id);
                let subscription = provider
                    .as_ref()
                    .and_then(|provider| provider.auth().oauth.as_ref())
                    .map(|oauth| oauth.is_subscription());
                AuthSelectorProvider {
                    id: credential.provider_id.clone(),
                    name: provider
                        .as_ref()
                        .map(|provider| provider.name().to_string())
                        .unwrap_or_else(|| credential.provider_id.clone()),
                    auth_type: credential.r#type,
                    method: None,
                    status: Some(AuthCheck {
                        source: Some("stored credential".to_string()),
                        r#type: credential.r#type,
                    }),
                    subscription,
                }
            })
            .collect();
        options.sort_by(|a, b| kiana_tui::autocomplete::locale_compare(&a.name, &b.name));
        Ok(options)
    }

    /// `findLoginProviderOptions(providerRef)`。
    ///
    /// 精确匹配：`js_trim` 后小写比较 id 或显示名，不做前缀/模糊匹配；全空白输入返回空
    /// 列表，调用方会因此打开 provider 选择器。
    fn find_login_provider_options(&self, provider_ref: &str) -> Vec<AuthSelectorProvider> {
        let normalized = js_trim(provider_ref).to_lowercase();
        if normalized.is_empty() {
            return Vec::new();
        }
        self.get_login_provider_options(None)
            .into_iter()
            .filter(|provider| {
                provider.id.to_lowercase() == normalized
                    || provider.name.to_lowercase() == normalized
            })
            .collect()
    }

    /// `handleLoginCommand(providerRef?)`。
    ///
    /// 分派规则（与 TS 逐条对应）：
    /// 1. 没有参数 → 认证方式选择器；
    /// 2. 参数精确命中一个选项 → 直接开始该选项的登录；
    /// 3. 命中多个选项但 id 相同（同一 provider 同时提供 OAuth 与 API key）→ 对这种 provider
    ///    打开认证方式选择器，让用户选其中一种；
    /// 4. 其余情况（没有命中，或命中多个不同 provider）→ 打开 provider 选择器，并把原始
    ///    参数作为搜索框初值。
    pub(crate) async fn handle_login_command(&mut self, provider_ref: Option<String>) {
        let Some(provider_ref) = provider_ref else {
            self.show_login_auth_type_selector(None);
            return;
        };
        let provider_options = self.find_login_provider_options(&provider_ref);
        if provider_options.len() == 1 {
            let option = provider_options[0].clone();
            self.start_provider_login(option, None).await;
            return;
        }
        if provider_options.len() > 1 {
            let unique_ids: std::collections::HashSet<&str> = provider_options
                .iter()
                .map(|provider| provider.id.as_str())
                .collect();
            if unique_ids.len() == 1 {
                self.show_login_auth_type_selector(Some(provider_options));
                return;
            }
        }
        self.show_login_provider_selector(None, Some(provider_ref));
    }

    /// `startProviderLogin(providerOption, onBack?)`。
    ///
    /// 按选项的 auth 类型分发：OAuth 走登录对话框；API key 只有在
    /// `ApiKeyAuth::has_login()` 为真时才弹输入对话框——否则该 provider 的密钥来自环境变量/
    /// 配置文件，改为显示 ambient 提示框（不做认证）。
    async fn start_provider_login(
        &mut self,
        provider_option: AuthSelectorProvider,
        on_back: Option<OnBack>,
    ) {
        match provider_option.auth_type {
            AuthType::OAuth => {
                self.show_login_dialog(
                    provider_option.id.clone(),
                    provider_option.name.clone(),
                    on_back,
                )
                .await;
            }
            AuthType::ApiKey => {
                let has_login = matches!(
                    &provider_option.method,
                    Some(AuthSelectorMethod::ApiKey(auth)) if auth.has_login()
                );
                if has_login {
                    self.show_api_key_login_dialog(
                        provider_option.id.clone(),
                        provider_option.name.clone(),
                        on_back,
                    )
                    .await;
                } else {
                    self.show_ambient_auth_dialog(provider_option, on_back);
                }
            }
        }
    }

    /// `showLoginAuthTypeSelector(providerOptions?)`。
    ///
    /// 顶层菜单（`providerOptions == None`）总是提供 OAuth 与 API key 两种方式，并额外把
    /// Radius 作为最后一个直达选项（标签带状态后缀，用带 shimmer 动画的
    /// `create_login_menu_selector`）；从 `/login <provider>` 进来时
    /// （`Some(options)`）只列该 provider 实际可用的方式，且只剩一种时直接开始登录。
    ///
    /// 选中后按标签反查认证方式；`onBack` 会重新打开同一层菜单（顶层或同一组 provider 的
    /// 方式列表），使用户取消登录对话框后回到这里。
    pub(crate) fn show_login_auth_type_selector(
        &mut self,
        provider_options: Option<Vec<AuthSelectorProvider>>,
    ) {
        // Radius 只由顶层菜单追加为直达选项：先从 OAuth provider 列表里找它，稍后作为
        // 最后一个选项追加。注意二层菜单（`/login <provider>`）按 provider 的 auth 类型
        // 过滤时 Radius 仍会作为普通条目一起列出——它的 `ProviderAuth` 同时含 oauth 与
        // api_key（见 `kiana-ai` 中 radius provider 的定义），只是没有带 shimmer 的直达
        // 选项；带 shimmer 的直达选项只在顶层菜单追加。
        let radius_option = if provider_options.is_none() {
            self.get_login_provider_options(Some(AuthType::OAuth))
                .into_iter()
                .find(|provider| provider.id == kiana_config::defaults::RADIUS_PROVIDER_ID)
        } else {
            None
        };
        // Radius 的完整标签 = 主文案 + 状态后缀。状态后缀由
        // `format_auth_selector_provider_status` 生成，可能带主题色转义；选择器回调只把
        // 这个完整字符串交回来，所以后面必须用同一个字符串比对才能认出 Radius 选项。
        let radius_text = radius_option
            .as_ref()
            .map(|option| format!("Sign in with {}", option.name));
        let radius_label = radius_option.as_ref().map(|option| {
            format!(
                "{}{}",
                radius_text.clone().unwrap_or_default(),
                format_auth_selector_provider_status(option)
            )
        });
        let oauth_login_label = provider_options.as_ref().and_then(|options| {
            options
                .iter()
                .find(|provider| provider.auth_type == AuthType::OAuth)
                .and_then(|provider| match &provider.method {
                    Some(AuthSelectorMethod::OAuth(oauth)) => {
                        oauth.login_label().map(str::to_string)
                    }
                    _ => None,
                })
        });
        // 标签同时是显示文本与选项身份（选中回调拿到的就是标签字符串）：OAuth 标签优先用
        // provider 自己声明的 loginLabel，否则用通用文案。
        let subscription_label =
            oauth_login_label.unwrap_or_else(|| "Sign in with an account".to_string());
        let api_key_label = "Sign in with an API key".to_string();
        // 同一认证方式可能由多个 provider 条目提供（例如两个 OAuth provider），用集合
        // 去重后菜单里每种方式只出现一次。
        let available_auth_types: std::collections::HashSet<AuthType> = match &provider_options {
            Some(options) => options.iter().map(|provider| provider.auth_type).collect(),
            None => [AuthType::OAuth, AuthType::ApiKey].into_iter().collect(),
        };
        let mut options: Vec<String> = Vec::new();
        if available_auth_types.contains(&AuthType::OAuth) {
            options.push(subscription_label.clone());
        }
        if available_auth_types.contains(&AuthType::ApiKey) {
            options.push(api_key_label.clone());
        }
        if let Some(radius_label) = &radius_label {
            options.push(radius_label.clone());
        }

        if options.is_empty() {
            self.show_status("No login methods available.");
            return;
        }

        // 只有一种可选方式（来自 `/login <provider>` 的过滤结果）：直接开始登录，
        // 不显示单选项菜单。
        if let Some(provider_options) = &provider_options
            && options.len() == 1
        {
            if let Some(provider_option) = provider_options.first() {
                let option = provider_option.clone();
                // `start_provider_login` 是 async，而本函数是同步的（调用方既可能是
                // `dispatch_submit`，也可能是选择器回调）：克隆模式句柄后放进本地任务，
                // 不在当前调用栈上 await。
                let mut mode = self.clone();
                tokio::task::spawn_local(async move {
                    mode.start_provider_login(option, None).await;
                });
            }
            return;
        }

        let title = provider_options
            .as_ref()
            .and_then(|options| options.first())
            .map(|provider| format!("Select authentication method for {}:", provider.name))
            .unwrap_or_else(|| "Select authentication method:".to_string());
        let request_render = self.request_render_callback();
        let provider_options_for_select = provider_options.clone();
        let radius_for_select = radius_option.clone();
        let radius_label_for_select = radius_label.clone();
        let radius_text_for_select = radius_text.clone();
        let subscription_label_for_select = subscription_label.clone();
        let weak = self.downgrade();
        self.show_selector(move |_mode, done| {
            let weak_select = weak.clone();
            let done_select = done.clone();
            let provider_options_select = provider_options_for_select.clone();
            let radius_select = radius_for_select.clone();
            let radius_label_select = radius_label_for_select.clone();
            let subscription_label_select = subscription_label_for_select.clone();
            let on_select = move |option: &str| {
                done_select.finish();
                let Some(inner) = weak_select.upgrade() else {
                    return;
                };
                let mode = InteractiveMode { inner };
                // Radius 直达选项：登录对话框取消时回到顶层菜单（不带 provider 过滤）。
                if radius_select.is_some() && option == radius_label_select.as_deref().unwrap_or("")
                {
                    if let Some(radius_option) = radius_select.clone() {
                        let mut mode = mode.clone();
                        tokio::task::spawn_local(async move {
                            mode.start_provider_login(
                                radius_option,
                                Some(Box::new(|mode: &mut InteractiveMode| {
                                    mode.show_login_auth_type_selector(None);
                                })),
                            )
                            .await;
                        });
                    }
                    return;
                }
                let auth_type = if option == subscription_label_select {
                    AuthType::OAuth
                } else {
                    AuthType::ApiKey
                };
                if let Some(provider_options) = provider_options_select.clone() {
                    if let Some(provider_option) = provider_options
                        .iter()
                        .find(|provider| provider.auth_type == auth_type)
                        .cloned()
                    {
                        let options_back = provider_options.clone();
                        let mut mode = mode.clone();
                        tokio::task::spawn_local(async move {
                            mode.start_provider_login(
                                provider_option,
                                Some(Box::new(move |mode: &mut InteractiveMode| {
                                    mode.show_login_auth_type_selector(Some(options_back.clone()));
                                })),
                            )
                            .await;
                        });
                    }
                    return;
                }
                let mut mode = mode.clone();
                tokio::task::spawn_local(async move {
                    mode.show_login_provider_selector(Some(auth_type), None);
                });
            };
            let weak_cancel = weak.clone();
            let on_cancel = move || {
                done.finish();
                if let Some(inner) = weak_cancel.upgrade() {
                    InteractiveMode { inner }.request_render();
                }
            };
            if let (Some(radius_label), Some(radius_text)) = (
                radius_label_for_select.clone(),
                radius_text_for_select.clone(),
            ) {
                let selector = create_login_menu_selector(
                    request_render.clone(),
                    &title,
                    options.clone(),
                    RadiusOption {
                        label: radius_label,
                        text: radius_text,
                    },
                    on_select,
                    on_cancel,
                );
                let shared = Rc::new(RefCell::new(selector));
                let dispose_handle = Rc::clone(&shared);
                SelectorCreation {
                    component: shared,
                    focus: SelectorFocus::Component,
                    // TS 两个分支都返回 `dispose: () => selector.dispose()`：Radius 菜单的
                    // dispose 还要停止 shimmer 定时器。
                    dispose: Some(Box::new(move || {
                        if let Ok(mut selector) = dispose_handle.try_borrow_mut() {
                            selector.dispose();
                        }
                    })),
                }
            } else {
                let selector = ExtensionSelectorComponent::new(
                    &title,
                    options.clone(),
                    on_select,
                    on_cancel,
                    ExtensionSelectorOptions::default(),
                );
                let shared = shared_component(selector);
                let dispose_handle = Rc::clone(&shared);
                SelectorCreation {
                    component: shared,
                    focus: SelectorFocus::Component,
                    dispose: Some(Box::new(move || {
                        if let Ok(mut selector) = dispose_handle.try_borrow_mut() {
                            selector.dispose();
                        }
                    })),
                }
            }
        });
    }

    /// `showLoginProviderSelector(authType?, initialSearchInput?)`。
    ///
    /// 列出该认证方式下的 provider 选项（搜索框初值来自 `/login <provider>` 的参数）。
    /// 没有可选 provider 时按 `auth_type` 显示三条固定文案之一。选中项要同时匹配 id 与
    /// auth 类型；取消时若来自认证方式菜单（`auth_type.is_some()`）则回到该菜单，否则只重绘。
    pub(crate) fn show_login_provider_selector(
        &mut self,
        auth_type: Option<AuthType>,
        initial_search_input: Option<String>,
    ) {
        let provider_options = self.get_login_provider_options(auth_type);
        if provider_options.is_empty() {
            let message = match auth_type {
                Some(AuthType::OAuth) => "No account providers available.",
                Some(AuthType::ApiKey) => "No API key providers available.",
                None => "No login providers available.",
            };
            self.show_status(message);
            return;
        }
        let weak = self.downgrade();
        self.show_selector(move |_mode, done| {
            let done_select = done.clone();
            let providers = provider_options.clone();
            let initial = initial_search_input.clone();
            let weak_select = weak.clone();
            let on_select = move |provider_id: &str, selected_auth_type: AuthType| {
                done_select.finish();
                let Some(inner) = weak_select.upgrade() else {
                    return;
                };
                let mut mode = InteractiveMode { inner };
                // 同一 provider 可能以 OAuth 与 API key 两个条目出现，必须同时匹配两者。
                let Some(provider_option) = providers
                    .iter()
                    .find(|provider| {
                        provider.id == provider_id && provider.auth_type == selected_auth_type
                    })
                    .cloned()
                else {
                    return;
                };
                let initial_back = initial.clone();
                tokio::task::spawn_local(async move {
                    mode.start_provider_login(
                        provider_option,
                        Some(Box::new(move |mode: &mut InteractiveMode| {
                            mode.show_login_provider_selector(auth_type, initial_back.clone());
                        })),
                    )
                    .await;
                });
            };
            let weak_cancel = weak.clone();
            let on_cancel = move || {
                done.finish();
                if let Some(inner) = weak_cancel.upgrade() {
                    let mut mode = InteractiveMode { inner };
                    // 从认证方式菜单进来的：取消应回到上一层菜单；直接 `/login <ref>`
                    // 进来的没有上一层，只重绘。
                    if auth_type.is_some() {
                        tokio::task::spawn_local(async move {
                            mode.show_login_auth_type_selector(None);
                        });
                    } else {
                        mode.request_render();
                    }
                }
            };
            let selector = OAuthSelectorComponent::new(
                AuthSelectorMode::Login,
                provider_options.clone(),
                on_select,
                on_cancel,
                initial_search_input.as_deref(),
            );
            SelectorCreation {
                component: shared_component(selector),
                focus: SelectorFocus::Component,
                dispose: None,
            }
        });
    }

    /// `showOAuthSelector(mode)`（`/logout` 走这里）。
    ///
    /// `"login"` 分支只是 TS 入口兼容（直接转到认证方式选择器），生产调用方只用
    /// `"logout"`（`dispatch_submit` 的 `/logout` 分支）。
    ///
    /// 登出前先用 15 秒超时列出已保存凭据：读取失败显示
    /// “Could not read stored credentials: …”；没有凭据则显示固定说明文案（`/logout` 只删
    /// `/login` 保存的凭据，环境变量与 models.json 不受影响）。
    pub(crate) async fn show_oauth_selector(&mut self, mode: &str) {
        if mode == "login" {
            self.show_login_auth_type_selector(None);
            return;
        }
        let provider_options = match self.get_logout_provider_options().await {
            Ok(options) => options,
            Err(error) => {
                self.show_error(&format!("Could not read stored credentials: {error}"));
                return;
            }
        };
        if provider_options.is_empty() {
            self.show_status(
                "No stored credentials to remove. /logout only removes credentials saved by /login; environment variables and models.json config are unchanged.",
            );
            return;
        }
        let weak = self.downgrade();
        self.show_selector(move |_mode, done| {
            let done_select = done.clone();
            let providers = provider_options.clone();
            let weak_select = weak.clone();
            let on_select = move |provider_id: &str, _auth_type: AuthType| {
                done_select.finish();
                let Some(inner) = weak_select.upgrade() else {
                    return;
                };
                let mut mode = InteractiveMode { inner };
                let Some(provider_option) = providers
                    .iter()
                    .find(|provider| provider.id == provider_id)
                    .cloned()
                else {
                    return;
                };
                tokio::task::spawn_local(async move {
                    mode.logout_provider(provider_option).await;
                });
            };
            let weak_cancel = weak.clone();
            let on_cancel = move || {
                done.finish();
                if let Some(inner) = weak_cancel.upgrade() {
                    InteractiveMode { inner }.request_render();
                }
            };
            let selector = OAuthSelectorComponent::new(
                AuthSelectorMode::Logout,
                provider_options.clone(),
                on_select,
                on_cancel,
                None,
            );
            SelectorCreation {
                component: shared_component(selector),
                focus: SelectorFocus::Component,
                dispose: None,
            }
        });
    }

    /// `/logout` 的选中处理器。
    ///
    /// 删除指定 provider 的持久化凭据（`ModelRuntime::logout`，同样带 15 秒超时），成功后
    /// 刷新 footer 的 provider 计数并按认证类型显示文案。失败分两类：`CredentialSync`
    /// 表示**凭据已经删除**、只是本地模型状态同步失败（必须用不同文案，否则用户会误以为
    /// 登出没生效）；其它错误统一显示 “Logout failed: …”。
    async fn logout_provider(&mut self, provider_option: AuthSelectorProvider) {
        let runtime = self.session().model_runtime().clone();
        let signal = AbortScope::new();
        let timeout_signal = signal.clone();
        tokio::task::spawn_local(async move {
            tokio::time::sleep(Duration::from_millis(15_000)).await;
            // `AbortSignal.timeout()` 的 reason：`TimeoutError`（Node 的固定文案）。
            timeout_signal.abort_with(kiana_ai::utils::abort::AbortReason {
                name: "TimeoutError".to_string(),
                message: "The operation was aborted due to timeout".to_string(),
            });
        });
        let result = runtime
            .logout(
                &provider_option.id,
                AuthOperationOptions {
                    signal: Some(signal),
                },
            )
            .await;
        match result {
            Ok(()) => {
                self.update_available_provider_count();
                let message = match provider_option.auth_type {
                    AuthType::OAuth => format!("Logged out of {}", provider_option.name),
                    AuthType::ApiKey => format!(
                        "Removed stored API key for {}. Environment variables and models.json config are unchanged.",
                        provider_option.name
                    ),
                };
                self.show_status(&message);
            }
            Err(error) => {
                let message = error.message();
                // CredentialSync 发生在凭据已持久化删除之后：文案必须区分，避免用户以为
                // 登出没有生效。
                if matches!(error, ModelRuntimeError::CredentialSync(_)) {
                    self.show_error(&format!(
                        "Credentials removed for {}, but local model state could not be synchronized: {message}",
                        provider_option.name
                    ));
                } else {
                    self.show_error(&format!("Logout failed: {message}"));
                }
            }
        }
    }

    /// `completeProviderAuthentication(providerId, providerName, authType, previousModel)`。
    ///
    /// 登录成功后的收尾分两段：
    /// 1. **立即**：不需要等目录刷新时直接 `finish_authentication`；需要等（`defer_selection`）
    ///    时先提示 “Refreshing model catalog…”，把选模型推迟到刷新之后；
    /// 2. **后台**：只刷新该 provider 的模型目录，15 秒超时，登录流程不等待它（#7027：
    ///    刷新卡住也不能拖住登录）。刷新结束后补一次 `finish_authentication`，但仅当会话与
    ///    当前模型都没被用户改过（`session_id` 相等且模型仍是 `previous_model`）。
    ///
    /// `defer_selection` 的三个条件：登录前是 unknown 占位模型、该 provider 有默认模型、
    /// 且当前可用快照里还没有这个默认模型——动态目录通常要完成认证后的网络刷新才会出现。
    async fn complete_provider_authentication(
        &mut self,
        provider_id: &str,
        provider_name: &str,
        auth_type: AuthType,
        previous_model: Option<kiana_ai::types::Model>,
    ) {
        let action_label = match auth_type {
            AuthType::OAuth => format!("Logged in to {provider_name}"),
            AuthType::ApiKey => format!("Saved API key for {provider_name}"),
        };
        let session = self.session();
        let defer_selection = crate::interactive_mode::is_unknown_model(previous_model.as_ref())
            && crate::interactive_mode::has_default_model_provider(provider_id)
            && !session
                .model_runtime()
                .get_available_snapshot()
                .iter()
                .any(|model| {
                    model.provider == provider_id
                        && Some(model.id.as_str())
                            == DEFAULT_MODEL_PER_PROVIDER.get(provider_id).copied()
                });

        // 目录为空是动态 provider 的常态：等后台刷新拿到模型后再自动选择默认模型。
        if defer_selection {
            self.show_status(&format!(
                "{action_label}. Credentials saved to {}. Refreshing model catalog…",
                kiana_core::config::get_auth_path().to_string_lossy()
            ));
        } else {
            self.finish_authentication(provider_id, &action_label, previous_model.clone())
                .await;
        }

        // TS：`void session.modelRuntime.refresh({ providers, signal }).then(...)`。
        // 登录流程**不等待**目录刷新（#7027）：刷新卡住时也必须立即完成登录。
        let weak = self.downgrade();
        let provider_id = provider_id.to_string();
        let action_label = action_label.clone();
        let previous_model = previous_model.clone();
        // TS 的 `this.session === session`：会话切换后不要用旧登录流程覆盖新会话。
        let session_id = session.session_id();
        let runtime = session.model_runtime().clone();
        tokio::task::spawn_local(async move {
            let signal = CancellationToken::new();
            let refresh_signal = AbortScope::from(signal.clone());
            let refresh = runtime.refresh(kiana_ai::models::ModelsRefreshOptions {
                allow_network: None,
                providers: Some(vec![provider_id.clone()]),
                force: None,
                signal: Some(refresh_signal),
            });
            tokio::pin!(refresh);
            let result = tokio::select! {
                result = &mut refresh => result,
                _ = tokio::time::sleep(Duration::from_millis(15_000)) => {
                    // TS：`setTimeout(() => controller.abort(), 15_000)`。refresh 在取消后
                    // resolve `{ aborted: true }`（超时警告），只有真正 reject 才走
                    // “could not be refreshed” 分支；这里保留它的结果。
                    signal.cancel();
                    refresh.await
                }
            };
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let mut mode = InteractiveMode { inner };
            match result {
                Ok(result) => {
                    // 刷新失败/超时只产生警告：登录已经成功，继续使用缓存目录。
                    if result.aborted {
                        mode.show_warning(&format!(
                            "{action_label}, but its model catalog refresh timed out; using cached models."
                        ));
                    } else if !result.errors.is_empty() {
                        mode.show_warning(&format!(
                            "{action_label}, but its model catalog could not be refreshed; using cached models."
                        ));
                    }
                    // TS：刷新期间用户可能已经切换会话或选择了模型，不要覆盖。
                    if defer_selection
                        && mode.session().session_id() == session_id
                        && mode.session().model() == previous_model
                    {
                        mode.finish_authentication(&provider_id, &action_label, previous_model)
                            .await;
                    }
                    mode.update_available_provider_count();
                    mode.inner.borrow().footer.borrow_mut().invalidate();
                    mode.request_render();
                }
                // TS 的 `.catch` 只显示警告，不执行 `.then` 的收尾逻辑。
                Err(error) => {
                    mode.show_warning(&format!(
                        "{action_label}, but its model catalog could not be refreshed: {error}"
                    ));
                }
            }
        });
    }

    /// `completeProviderAuthentication` 的 `finishAuthentication()`。
    ///
    /// 只在登录前是 unknown 占位模型时才尝试切换默认模型（否则用户已有明确选择，不能动）。
    /// 分支与对应文案：
    /// 1. `llama.cpp` → 专门指引（模型需要先用 `/llama` 从本地加载）；
    /// 2. provider 没有默认模型 → 要求用户 `/model`；
    /// 3. 快照里没有该 provider 的模型 → 要求用户 `/model`；
    /// 4. 否则按 `DEFAULT_MODEL_PER_PROVIDER` 找默认模型（Radius 例外：默认模型缺失时退回
    ///    目录里的第一个），找到就 `set_model(..., persist = true)`，失败转成错误文案。
    ///
    /// 无论是否选中模型都会刷新 provider 计数、footer 与编辑器边框；最后按结果二选一：
    /// 有选中模型 → 状态里带上模型 id，并对该模型做 Anthropic 警告检查；没有 → 状态只报
    /// “凭据已保存”，有 `selection_error` 就显示错误，否则用会话当前模型做警告检查。
    async fn finish_authentication(
        &mut self,
        provider_id: &str,
        action_label: &str,
        previous_model: Option<kiana_ai::types::Model>,
    ) {
        let mut selected_model: Option<kiana_ai::types::Model> = None;
        let mut selection_error: Option<String> = None;
        // 用户登录前已有明确模型时保持不动；只有 unknown 占位模型才需要自动选。
        if crate::interactive_mode::is_unknown_model(previous_model.as_ref()) {
            let available_models = self.session().model_runtime().get_available_snapshot();
            let provider_models: Vec<kiana_ai::types::Model> = available_models
                .into_iter()
                .filter(|model| model.provider == provider_id)
                .collect();
            if provider_id == "llama.cpp" {
                selection_error = Some(crate::interactive_mode::llama_cpp_post_login_guidance(
                    action_label,
                    provider_models.len(),
                ));
            } else if !crate::interactive_mode::has_default_model_provider(provider_id) {
                selection_error = Some(format!(
                    "{action_label}, but no default model is configured for provider \"{provider_id}\". Use /model to select a model."
                ));
            } else if provider_models.is_empty() {
                selection_error = Some(format!(
                    "{action_label}, but no models are available for that provider. Use /model to select a model."
                ));
            } else {
                let default_model_id = DEFAULT_MODEL_PER_PROVIDER
                    .get(provider_id)
                    .cloned()
                    .unwrap_or_default();
                selected_model = provider_models
                    .iter()
                    .find(|model| model.id == default_model_id)
                    .cloned()
                    .or_else(|| {
                        // Radius 的目录随账号变化，默认模型可能不存在：退回目录第一个。
                        if provider_id == "radius" {
                            provider_models.first().cloned()
                        } else {
                            None
                        }
                    });
                match selected_model.clone() {
                    None => {
                        selection_error = Some(format!(
                            "{action_label}, but its default model \"{default_model_id}\" is not available. Use /model to select a model."
                        ));
                    }
                    Some(model) => {
                        let result = self
                            .session()
                            .set_model(
                                model.clone(),
                                kiana_session::agent_session::ModelMutationOptions {
                                    persist: Some(true),
                                },
                            )
                            .await;
                        if let Err(error) = result {
                            // 切换失败时清掉“已选中”状态，让最终文案改走
                            // `selection_error`，避免同时报告选中的模型 id 与失败原因。
                            selected_model = None;
                            selection_error = Some(format!(
                                "{action_label}, but selecting its default model failed: {error}. Use /model to select a model."
                            ));
                        }
                    }
                }
            }
        }

        // 公共收尾：provider 计数、footer 与编辑器边框（成功/失败分支都会走）。
        self.update_available_provider_count();
        self.inner.borrow().footer.borrow_mut().invalidate();
        self.update_editor_border_color();
        if let Some(selected_model) = selected_model {
            self.show_status(&format!(
                "{action_label}. Selected {}. Credentials saved to {}",
                selected_model.id,
                kiana_core::config::get_auth_path().to_string_lossy()
            ));
            self.maybe_warn_about_anthropic_subscription_auth_for(Some(selected_model))
                .await;
        } else {
            self.show_status(&format!(
                "{action_label}. Credentials saved to {}",
                kiana_core::config::get_auth_path().to_string_lossy()
            ));
            match selection_error {
                Some(error) => self.show_error(&error),
                None => self.maybe_warn_about_anthropic_subscription_auth().await,
            }
        }
    }

    /// `showAmbientAuthDialog(providerOption, onBack?)`。
    ///
    /// 用于凭据来自进程外部（环境变量、AWS 配置等）且没有交互式 login 的 provider：只显示
    /// 说明对话框，不发起认证。标题是 “<provider> setup”，正文是
    /// “<method name 或 Authentication> is configured outside <APP_NAME>.”，带关闭提示。
    /// 用户按 Esc 关闭时对话框触发 `on_complete`，恢复编辑器后再执行 `on_back` 返回上一层。
    fn show_ambient_auth_dialog(
        &mut self,
        provider_option: AuthSelectorProvider,
        on_back: Option<OnBack>,
    ) {
        // TS 的 onComplete：恢复编辑器并回调 `onBack`。
        let weak_complete = self.downgrade();
        // on_back 放在共享槽里：回调是 FnMut、不能 move 出，take() 保证最多执行一次。
        let back_slot: Rc<RefCell<Option<OnBack>>> = Rc::new(RefCell::new(on_back));
        let back_for_complete = Rc::clone(&back_slot);
        let on_complete: crate::components::login_dialog::LoginCompleteCallback =
            Box::new(move |_success, _message| {
                let Some(inner) = weak_complete.upgrade() else {
                    return;
                };
                let back = back_for_complete.clone();
                // 该回调由对话框的 handle_input 同步触发；立即清空槽位会 drop 正在分发输入
                // 的组件，所以延后到下一个本地任务（同 commands/mod.rs 的 run_after_dispatch）。
                tokio::task::spawn_local(async move {
                    let mut mode = InteractiveMode { inner };
                    mode.restore_editor_after_login();
                    if let Some(mut on_back) = back.borrow_mut().take() {
                        on_back(&mut mode);
                    }
                });
            });
        let dialog = self.create_login_dialog_with_complete(
            &provider_option.id,
            &provider_option.name,
            Some(format!("{} setup", provider_option.name)),
            on_complete,
        );
        let method_name = provider_option
            .method
            .as_ref()
            .map(|method| method.name().to_string())
            .unwrap_or_else(|| "Authentication".to_string());
        dialog.borrow_mut().show_info(
            &format!(
                "{method_name} is configured outside {}.",
                kiana_core::config::APP_NAME
            ),
            &[],
            true,
        );
        self.mount_login_dialog(&dialog);
    }

    /// `showApiKeyLoginDialog(providerId, providerName, onBack?)`。
    ///
    /// API key 登录：挂对话框 → `login_provider` → 恢复编辑器 → 成功交给
    /// `complete_provider_authentication`。失败分三类：`CredentialSync`（key 已保存但本地
    /// 状态同步失败）、`"Login cancelled"`（回 `on_back` 重开选择器）、其它（显示失败原因）。
    ///
    /// Bedrock 额外在对话框里展示 `providers.md` 的路径：它的凭据也可以来自 AWS
    /// profile、IAM key 或 role，而不只是这里输入的 API key。
    async fn show_api_key_login_dialog(
        &mut self,
        provider_id: String,
        provider_name: String,
        on_back: Option<OnBack>,
    ) {
        let previous_model = self.session().model();
        let dialog = self.create_login_dialog(&provider_id, &provider_name, None);
        if provider_id == "amazon-bedrock" {
            // Bedrock 还可以用 AWS profile/IAM key/role，本对话框只保存 API key，因此额外
            // 指出文档位置。
            let docs_path = kiana_core::config::get_docs_path().join("providers.md");
            let lines = [
                fg(
                    kiana_theme::theme_tokens::ThemeColor::Text,
                    "You can also use an AWS profile, IAM keys, or role-based credentials.",
                ),
                fg(kiana_theme::theme_tokens::ThemeColor::Muted, "See:"),
                fg(
                    kiana_theme::theme_tokens::ThemeColor::Accent,
                    &format!("  {}", docs_path.to_string_lossy()),
                ),
            ];
            dialog
                .borrow_mut()
                .show_details(&lines.iter().map(String::as_str).collect::<Vec<_>>());
        }
        self.mount_login_dialog(&dialog);

        let result = self
            .login_provider(&dialog, &provider_id, &provider_name, AuthType::ApiKey)
            .await;
        self.restore_editor_after_login();
        match result {
            Ok(()) => {
                self.complete_provider_authentication(
                    &provider_id,
                    &provider_name,
                    AuthType::ApiKey,
                    previous_model,
                )
                .await;
            }
            Err(error) => {
                // 取消没有独立的错误类型，只按固定 message 文本识别（TS 同样按字符串）。
                if matches!(error, ModelRuntimeError::CredentialSync(_)) {
                    self.show_error(&format!(
                        "Saved API key for {provider_name}, but local model state could not be synchronized: {}",
                        error.message()
                    ));
                } else if error.message() == "Login cancelled" {
                    if let Some(mut on_back) = on_back {
                        on_back(self);
                    }
                } else {
                    self.show_error(&format!(
                        "Failed to save API key for {provider_name}: {}",
                        error.message()
                    ));
                }
            }
        }
    }

    /// `showLoginDialog(providerId, providerName, onBack?)`（OAuth）。
    ///
    /// OAuth 登录主流程：挂对话框 → `login_provider`（浏览器授权/设备码等提示都经 UI 桥
    /// 展示）→ 恢复编辑器 → 成功交给 `complete_provider_authentication`；Radius 登录成功后
    /// 再追加 `offer_radius_mcp_server`。失败分支与 API key 版本相同（`CredentialSync` /
    /// 取消回 `on_back` / 其它错误）。
    async fn show_login_dialog(
        &mut self,
        provider_id: String,
        provider_name: String,
        on_back: Option<OnBack>,
    ) {
        let previous_model = self.session().model();
        let dialog = self.create_login_dialog(&provider_id, &provider_name, None);
        self.mount_login_dialog(&dialog);
        let result = self
            .login_provider(&dialog, &provider_id, &provider_name, AuthType::OAuth)
            .await;
        self.restore_editor_after_login();
        match result {
            Ok(()) => {
                self.complete_provider_authentication(
                    &provider_id,
                    &provider_name,
                    AuthType::OAuth,
                    previous_model,
                )
                .await;
                if provider_id == kiana_config::defaults::RADIUS_PROVIDER_ID {
                    // Radius 登录成功后提议把 Radius MCP 服务器指向这次登录（见下方实现）。
                    self.offer_radius_mcp_server(&provider_id, &provider_name);
                }
            }
            Err(error) => {
                // 同 API key 分支：取消按固定 message 文本识别。
                if matches!(error, ModelRuntimeError::CredentialSync(_)) {
                    self.show_error(&format!(
                        "Logged in to {provider_name}, but local model state could not be synchronized: {}",
                        error.message()
                    ));
                } else if error.message() == "Login cancelled" {
                    if let Some(mut on_back) = on_back {
                        on_back(self);
                    }
                } else {
                    self.show_error(&format!(
                        "Failed to login to {provider_name}: {}",
                        error.message()
                    ));
                }
            }
        }
    }

    /// `loginProvider(dialog, providerId, providerName, method)`。
    ///
    /// 把登录对话框接到 runtime 的登录流程：
    /// 1. 在程序状态里登记名为 `"login"` 的阻塞项（Auth 类别），结束前必须清除；
    /// 2. 建无界通道与 `AuthUiBridge`；对话框的 `CancellationToken` 经 `AbortScope::from`
    ///    包装后共享同一取消状态，用户按 Esc 会同时中止 runtime 侧的流程；
    /// 3. 起 UI 循环后 await `ModelRuntime::login`：期间 Prompt 经通道往返，Notify 单向推送；
    /// 4. 结束后 drop 发送端关闭通道并清除阻塞项，返回值统一映射成 `Result<(), _>`。
    ///
    /// `get_device_id` 是回调而非常量：只有需要设备 id 的 OAuth 流程才会实际调用它。
    #[allow(
        clippy::result_large_err,
        reason = "ModelRuntimeError 是 kiana-config 的公开错误契约，形状不能改"
    )]
    async fn login_provider(
        &mut self,
        dialog: &Rc<RefCell<LoginDialogComponent>>,
        provider_id: &str,
        provider_name: &str,
        auth_type: AuthType,
    ) -> Result<(), ModelRuntimeError> {
        // 阻塞项按 source 去重；同名的 `None` 在函数结尾清除。
        self.inner.borrow_mut().program_status.set_blocked(
            "login",
            Some(crate::program_status_reporter::BlockedStatus {
                kind: kiana_tui::terminal_colors::ProgramStatusKind::Auth,
                message: format!("Log in to {provider_name}"),
            }),
        );

        // 无界通道连接 runtime 侧（可能在其他线程）与本地 UI 循环。
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let signal = AbortScope::from(dialog.borrow().signal());
        let bridge = AuthUiBridge {
            signal,
            sender: sender.clone(),
        };
        let container = Rc::clone(&self.inner.borrow().editor_container);
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        spawn_auth_ui_loop(
            Rc::clone(dialog),
            container,
            renderer,
            receiver,
            provider_id.to_string(),
        );

        let settings_manager = self.settings_manager();
        let runtime = self.session().model_runtime().clone();
        let result = runtime
            .login(
                provider_id,
                auth_type,
                Arc::new(bridge),
                LoginOptions {
                    get_device_id: Some(Arc::new(move || {
                        settings_manager.get_or_create_device_id()
                    })),
                    agent_name: None,
                },
            )
            .await;
        // 关闭通道：UI 循环处理完当前消息后退出（已排队的事件仍会被处理）。
        drop(sender);
        self.inner
            .borrow_mut()
            .program_status
            .set_blocked("login", None);
        result.map(|_| ())
    }

    /// 创建登录对话框（共享句柄）。
    ///
    /// 默认 `on_complete` 是空操作：OAuth / API key 流程在 `login_provider` 返回后自行收尾。
    fn create_login_dialog(
        &self,
        provider_id: &str,
        provider_name: &str,
        title_override: Option<String>,
    ) -> Rc<RefCell<LoginDialogComponent>> {
        self.create_login_dialog_with_complete(
            provider_id,
            provider_name,
            title_override,
            Box::new(|_success, _message| {}),
        )
    }

    /// 创建登录对话框（自定义 `onComplete`）。
    ///
    /// `title_override` 为 `None` 时组件用 “Login to <providerName>”；`on_complete` 在该
    /// 对话框被用户取消（取消键为 `tui.select.cancel`，默认 Esc / Ctrl+C）时收到
    /// `(false, Some("Login cancelled"))`，正常完成路径不会调用它。
    fn create_login_dialog_with_complete(
        &self,
        provider_id: &str,
        provider_name: &str,
        title_override: Option<String>,
        on_complete: crate::components::login_dialog::LoginCompleteCallback,
    ) -> Rc<RefCell<LoginDialogComponent>> {
        let request_render = self.request_render_callback();
        Rc::new(RefCell::new(LoginDialogComponent::new(
            request_render,
            provider_id,
            on_complete,
            Some(provider_name),
            title_override.as_deref(),
        )))
    }

    /// 把对话框挂到编辑器槽位并聚焦。
    ///
    /// 会清空编辑器槽位（登录期间编辑器不可见），结束后由 `restore_editor_after_login` 放回。
    fn mount_login_dialog(&mut self, dialog: &Rc<RefCell<LoginDialogComponent>>) {
        let container = Rc::clone(&self.inner.borrow().editor_container);
        let shared: kiana_tui::layout_node::SharedComponent = dialog.clone();
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        mount_shared_component(&container, &renderer, shared);
    }

    /// `restoreEditor()`（登录结束）。
    fn restore_editor_after_login(&mut self) {
        self.restore_editor_into_slot();
        self.request_render();
    }

    /// `offerRadiusMcpServer(providerId, providerName)`。
    ///
    /// Radius 登录成功后，提议把全局 `mcp.json` 里的 Radius MCP 服务器指向这次登录：
    /// 1. 只读全局层（`project_trusted = false`，不读项目 `.kiana/mcp.json`）；
    /// 2. 按 URL（忽略末尾 `/`）找已有服务器：已存在且 `auth.provider` 就是本 provider →
    ///    直接返回，不再询问；存在但认证不同 → 复用名称与原配置，只替换 `auth`；不存在 →
    ///    新建 HTTP 配置，名称默认 `"radius"`，若已被别的服务器占用则用 `"radius-mcp"`；
    /// 3. 选择 “Yes” 后写回 `mcp.json` 并触发 `/reload`（MCP 扩展在会话启动/重载时读取
    ///    该文件）；写失败只显示错误，不改内存状态。
    ///
    /// `auth` 与 `oauth` 互斥：写入 `auth` 时必须清空 `oauth`（TS：`delete config.oauth`）。
    fn offer_radius_mcp_server(&mut self, provider_id: &str, provider_name: &str) {
        use kiana_builtin_extensions::mcp::config::{
            LoadMcpConfigOptions, McpServerConfig, add_mcp_server_config, load_mcp_config,
        };
        let agent_dir = kiana_core::config::get_agent_dir();
        let mcp_path = agent_dir.join("mcp.json");
        let normalize_url = |url: &str| url.trim_end_matches('/').to_string();
        // 只读全局 mcp.json（project_trusted = false）；写回的目标也是这个文件。
        let loaded = load_mcp_config(LoadMcpConfigOptions {
            agent_dir: agent_dir.clone(),
            cwd: std::path::PathBuf::from(self.session_manager().get_cwd()),
            project_trusted: false,
        });
        // URL 比较忽略末尾的 `/`。
        let existing = loaded.servers.iter().find(|server| {
            matches!(&server.config, McpServerConfig::Http(config)
                if normalize_url(&config.url) == normalize_url(kiana_config::defaults::RADIUS_MCP_URL.as_str()))
        });
        // 已经指向本 provider 的登录：无需再次询问。
        if let Some(existing) = existing
            && let McpServerConfig::Http(config) = &existing.config
            && config.auth.as_ref().map(|auth| auth.provider.as_str()) == Some(provider_id)
        {
            return;
        }
        // 名称已被无关服务器占用时改用 radius-mcp，避免覆盖同名条目。
        let mut name = existing
            .map(|server| server.name.clone())
            .unwrap_or_else(|| "radius".to_string());
        if existing.is_none() && loaded.servers.iter().any(|server| server.name == name) {
            name = "radius-mcp".to_string();
        }
        let auth = kiana_core::mcp_servers::McpAuthConfig {
            provider: provider_id.to_string(),
            extra: indexmap::IndexMap::new(),
            input_key_order: vec!["provider".to_string()],
        };
        // 复用已有 HTTP 配置，只替换 auth；auth 与 oauth 互斥，写入 auth 时清空 oauth。
        let config = match existing.map(|server| server.config.clone()) {
            Some(McpServerConfig::Http(mut http)) => {
                http.auth = Some(auth);
                http.oauth = None;
                McpServerConfig::Http(http)
            }
            _ => McpServerConfig::Http(kiana_core::mcp_servers::McpHttpServerConfig {
                url: kiana_config::defaults::RADIUS_MCP_URL.as_str().to_string(),
                auth: Some(auth),
                ..Default::default()
            }),
        };
        let mcp_path_label = mcp_path.to_string_lossy().into_owned();
        let mcp_path_label_for_title = mcp_path_label.clone();
        let weak = self.downgrade();
        self.show_selector(move |_mode, done| {
            let weak_select = weak.clone();
            let done_select = done.clone();
            let config = config.clone();
            let mcp_path = mcp_path.clone();
            let mcp_path_label = mcp_path_label.clone();
            let name_for_select = name.clone();
            let on_select = move |option: &str| {
                done_select.finish();
                if option != "Yes" {
                    return;
                }
                // 写盘失败只显示错误；成功后重载，让 MCP 扩展重新读取 mcp.json。
                if let Err(error) = add_mcp_server_config(&mcp_path, &name_for_select, &config) {
                    if let Some(inner) = weak_select.upgrade() {
                        let mut mode = InteractiveMode { inner };
                        mode.show_error(&format!("Could not update {mcp_path_label}: {error}"));
                    }
                    return;
                }
                if let Some(inner) = weak_select.upgrade() {
                    let mut mode = InteractiveMode { inner };
                    tokio::task::spawn_local(async move {
                        mode.handle_reload_command().await;
                    });
                }
            };
            let weak_cancel = weak.clone();
            let on_cancel = move || {
                done.finish();
                if let Some(inner) = weak_cancel.upgrade() {
                    InteractiveMode { inner }.request_render();
                }
            };
            let title = format!("Configure {provider_name} MCP in {mcp_path_label_for_title}?");
            let selector = ExtensionSelectorComponent::new(
                &title,
                vec!["Yes".to_string(), "No".to_string()],
                on_select,
                on_cancel,
                ExtensionSelectorOptions::default(),
            );
            let shared = shared_component(selector);
            let dispose_handle = Rc::clone(&shared);
            SelectorCreation {
                component: shared,
                focus: SelectorFocus::Component,
                dispose: Some(Box::new(move || {
                    if let Ok(mut selector) = dispose_handle.try_borrow_mut() {
                        selector.dispose();
                    }
                })),
            }
        });
    }

    // --- 测试接缝 ---------------------------------------------------------
    // 集成测试在 crate 外，无法调用 `pub(crate)`/私有方法；这些薄转发用
    // `#[doc(hidden)] pub` 暴露给 tests/，不属于公开 API，生产代码不要调用。
    /// `maybeWarnAboutAnthropicSubscriptionAuth` 的测试接缝。
    #[doc(hidden)]
    pub async fn test_maybe_warn_about_anthropic_subscription_auth(
        &mut self,
        model: Option<kiana_ai::types::Model>,
    ) {
        self.maybe_warn_about_anthropic_subscription_auth_for(model)
            .await;
    }

    /// `showAmbientAuthDialog` 的测试接缝（无交互式 login 的 provider）。
    #[doc(hidden)]
    pub fn test_show_ambient_auth_dialog(&mut self, provider_id: &str, provider_name: &str) {
        self.show_ambient_auth_dialog(
            AuthSelectorProvider {
                id: provider_id.to_string(),
                name: provider_name.to_string(),
                auth_type: AuthType::ApiKey,
                method: None,
                status: None,
                subscription: None,
            },
            None,
        );
    }

    /// `showTrustSelector` 的测试接缝。
    #[doc(hidden)]
    pub fn test_show_trust_selector(&mut self) {
        self.show_trust_selector();
    }

    /// `handleLoginCommand` 的测试接缝。
    #[doc(hidden)]
    pub async fn test_handle_login_command(&mut self, provider_ref: Option<String>) {
        self.handle_login_command(provider_ref).await;
    }

    /// `showOAuthSelector` 的测试接缝。
    #[doc(hidden)]
    pub async fn test_show_oauth_selector(&mut self, mode: &str) {
        self.show_oauth_selector(mode).await;
    }

    /// `getLoginProviderOptions` 的测试接缝。
    #[doc(hidden)]
    pub fn test_get_login_provider_options(
        &self,
        auth_type: Option<AuthType>,
    ) -> Vec<AuthSelectorProvider> {
        self.get_login_provider_options(auth_type)
    }

    /// `getAuthCredential` 的测试接缝（`cli/auth-command.ts` 的本地 helper）。
    #[doc(hidden)]
    pub fn test_get_auth_credential(auth: &kiana_ai::auth::types::AuthResult) -> Option<String> {
        get_auth_credential(auth)
    }

    /// `completeProviderAuthentication` 的测试接缝。
    #[doc(hidden)]
    pub async fn test_complete_provider_authentication(
        &mut self,
        provider_id: &str,
        provider_name: &str,
        auth_type: AuthType,
        previous_model: Option<kiana_ai::types::Model>,
    ) {
        self.complete_provider_authentication(
            provider_id,
            provider_name,
            auth_type,
            previous_model,
        )
        .await;
    }

    /// `getLogoutProviderOptions` 的测试接缝。
    #[doc(hidden)]
    pub async fn test_get_logout_provider_options(
        &self,
    ) -> Result<Vec<AuthSelectorProvider>, String> {
        self.get_logout_provider_options().await
    }

    /// `login_provider_options` 的测试接缝。
    #[doc(hidden)]
    pub fn test_login_provider_options(
        runtime: &ModelRuntime,
        auth_type: Option<AuthType>,
    ) -> Vec<AuthSelectorProvider> {
        login_provider_options(runtime, auth_type)
    }
}

/// TS 的本地 `getAuthCredential(auth)`（`cli/auth-command.ts`）：
/// `auth?.auth.apiKey`（truthiness），否则大小写不敏感地取 `authorization` 头并按
/// `/^Bearer\s+(.+)$/iu` 提取 token。
///
/// 边界（均有测试 `auth_credential_matches_ts_bearer_header_semantics` 覆盖）：
/// - 非空 `apiKey` 直接返回；空串按 JS falsy 处理，继续看 headers；
/// - headers 只取**第一个**名称（小写后）等于 `authorization` 的条目；
/// - 值必须整体匹配 `^Bearer\s+(.+)$`（前缀大小写不敏感，`\s` 可包含制表符等空白，
///   捕获组是剩余全部文本，内部空格保留）；不匹配返回 `None`。
///
/// 本文件里没有生产调用点：它对应 TS `cli/auth-command.ts` 的本地 helper，这里只为测试
/// 接缝 `test_get_auth_credential` 保留。同 crate 的 `crate::session_share::get_auth_credential`
/// 是另一份对应同一 TS helper 的实现，生产路径（`session_share.rs`、`bug_report.rs`）
/// 使用的是那一份。
fn get_auth_credential(auth: &kiana_ai::auth::types::AuthResult) -> Option<String> {
    if let Some(api_key) = &auth.auth.api_key
        && !api_key.is_empty()
    {
        return Some(api_key.clone());
    }
    let authorization = auth
        .auth
        .headers
        .as_ref()?
        .iter()
        .find(|(name, _)| name.to_lowercase() == "authorization")
        .and_then(|(_, value)| value.as_deref())?;
    static BEARER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?iu)^Bearer\s+(.+)$").expect("bearer credential regex is valid")
    });
    BEARER
        .captures(authorization)
        .and_then(|captures| captures.get(1))
        .map(|token| token.as_str().to_string())
}

/// `getLoginProviderOptions(authType?)`（纯函数版本，供自动补全的 `Send + Sync` 闭包使用）。
///
/// 遍历 `ModelRuntime::get_providers()`，为每个 provider 最多生成两个条目（OAuth 与
/// API key，取决于它的 `auth()` 声明；给了 `auth_type` 就只生成对应类型）。`status` 只在
/// provider 已配置时出现：
/// - `source` 优先用 `AuthStatus.label`（环境变量名等），否则把来源枚举序列化成它的
///   snake_case 字符串（`"runtime"`、`"stored"`、`"environment"`、`"fallback"`、
///   `"models_json_key"`、`"models_json_command"`）；
/// - `type` 是**当前生效**的认证方式（`is_using_oauth`），不是条目的 `auth_type`：两者不
///   一致时选择器会显示 “<另一种方式> configured” 的警示。
///
/// 结果按显示名做 `localeCompare` 排序；`sort_by` 是稳定排序，同名条目保持遍历顺序。
pub(crate) fn login_provider_options(
    model_runtime: &ModelRuntime,
    auth_type: Option<AuthType>,
) -> Vec<AuthSelectorProvider> {
    let mut options: Vec<AuthSelectorProvider> = Vec::new();
    for provider in model_runtime.get_providers() {
        let auth_status = model_runtime.get_provider_auth_status(provider.id());
        // `status` 描述当前生效的认证；`is_using_oauth` 决定它的 type，可能与条目自身的
        // auth_type 不同（组件会据此提示“另一种方式已配置”）。
        let status = auth_status.configured.then(|| AuthCheck {
            source: auth_status.label.clone().or_else(|| {
                auth_status.source.map(|source| {
                    serde_json::to_value(source)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_string))
                        .unwrap_or_default()
                })
            }),
            r#type: if model_runtime.is_using_oauth(provider.id()) {
                AuthType::OAuth
            } else {
                AuthType::ApiKey
            },
        });
        let subscription = provider
            .auth()
            .oauth
            .as_ref()
            .map(|oauth| oauth.is_subscription());
        if (auth_type.is_none() || auth_type == Some(AuthType::OAuth))
            && let Some(oauth) = provider.auth().oauth.as_ref()
        {
            options.push(AuthSelectorProvider {
                id: provider.id().to_string(),
                name: provider.name().to_string(),
                auth_type: AuthType::OAuth,
                method: Some(AuthSelectorMethod::OAuth(Arc::clone(oauth))),
                status: status.clone(),
                subscription,
            });
        }
        if (auth_type.is_none() || auth_type == Some(AuthType::ApiKey))
            && let Some(api_key) = provider.auth().api_key.as_ref()
        {
            options.push(AuthSelectorProvider {
                id: provider.id().to_string(),
                name: provider.name().to_string(),
                auth_type: AuthType::ApiKey,
                method: Some(AuthSelectorMethod::ApiKey(Arc::clone(api_key))),
                status: status.clone(),
                subscription,
            });
        }
    }
    options.sort_by(|a, b| kiana_tui::autocomplete::locale_compare(&a.name, &b.name));
    options
}

/// 把共享组件挂进容器并聚焦最后一项。
///
/// 会清空 `container`（这里是编辑器槽位）再挂载组件代理，焦点设到新组件的地址并请求渲染；
/// 之后由调用方负责把编辑器/对话框重新挂回去。
fn mount_shared_component(
    container: &Rc<RefCell<Container>>,
    renderer: &Rc<RefCell<crate::tui_renderer::InteractiveRenderer>>,
    component: kiana_tui::layout_node::SharedComponent,
) {
    let address = {
        let mut container = container.borrow_mut();
        container.clear();
        container.add_child(Box::new(SharedComponentProxy::new(component)));
        container
            .children
            .last()
            .map(|child| component_address(child.as_ref()))
    };
    if let Some(address) = address {
        let mut ui = InteractiveTuiReference::new(Rc::clone(renderer));
        ui.set_focus_to_address(address);
        ui.request_render(false);
    }
}

/// 登录对话框的 UI 循环（在 UI 线程上驱动对话框）。
///
/// 消息串行处理：每个 `Prompt` 先 await 用户输入，再经 oneshot 把结果回给
/// `AuthUiBridge::prompt`（因此同一时刻只有一个提示占用对话框）；`Notify` 只更新对话框，
/// 不产生应答。所有发送端 drop 后 `recv()` 返回 `None`，任务自然结束。
///
/// `provider_id` 只继续传给 `show_auth_select`：Radius 的选项提示会据此追加登录说明。
fn spawn_auth_ui_loop(
    dialog: Rc<RefCell<LoginDialogComponent>>,
    container: Rc<RefCell<Container>>,
    renderer: Rc<RefCell<crate::tui_renderer::InteractiveRenderer>>,
    mut receiver: UnboundedReceiver<AuthUiMessage>,
    provider_id: String,
) {
    tokio::task::spawn_local(async move {
        while let Some(message) = receiver.recv().await {
            match message {
                AuthUiMessage::Notify(event) => notify_auth_dialog(&dialog, &event),
                AuthUiMessage::Prompt { request, respond } => {
                    let result =
                        show_auth_prompt_ui(&dialog, request, &container, &renderer, &provider_id)
                            .await;
                    let _ = respond.send(result);
                }
            }
        }
    });
}

/// `notifyAuthDialog(dialog, event)`。
///
/// 映射：`auth_url` → 展示 URL/说明；`device_code` → 设备码行并追加 “Waiting for
/// authentication...”（`*_on_confirm` 为 `None` 时组件按默认 `true` 处理）；`info` → 文本
/// 与链接（不带关闭提示）；`progress` → 进度行。
fn notify_auth_dialog(dialog: &Rc<RefCell<LoginDialogComponent>>, event: &AuthEvent) {
    match event {
        AuthEvent::AuthUrl { url, instructions } => {
            dialog.borrow_mut().show_auth(url, instructions.as_deref());
        }
        AuthEvent::DeviceCode {
            user_code,
            verification_uri,
            open_browser_on_confirm,
            copy_code_on_confirm,
            interval_seconds,
            expires_in_seconds,
        } => {
            dialog.borrow_mut().show_device_code(OAuthDeviceCodeInfo {
                user_code: user_code.clone(),
                verification_uri: verification_uri.clone(),
                open_browser_on_confirm: *open_browser_on_confirm,
                copy_code_on_confirm: *copy_code_on_confirm,
                interval_seconds: *interval_seconds,
                expires_in_seconds: *expires_in_seconds,
            });
            dialog
                .borrow_mut()
                .show_waiting("Waiting for authentication...");
        }
        AuthEvent::Info { message, links } => {
            dialog
                .borrow_mut()
                .show_info(message, links.as_deref().unwrap_or(&[]), false);
        }
        AuthEvent::Progress { message } => {
            dialog.borrow_mut().show_progress(message);
        }
    }
}

/// `showAuthPrompt(dialog, prompt, providerId)`。
///
/// 四类提示的映射：`select` → `show_auth_select`（临时占用编辑器槽位，选完挂回对话框）；
/// `manual_code` → `show_manual_input`（清空输入框，用于粘贴回调 URL/码）；`text` / `secret`
/// → `show_prompt`（追加内容，保留已显示的授权 URL）。
///
/// 取消有两个来源：请求自身的 `signal`（外部事件先到，例如回调服务器已收到 code）与整个
/// 登录的取消（对话框关闭会让输入 receiver 以 `LoginCancelledError` 结束）。两者都转成
/// `AuthError("Login cancelled")`，不把 `AbortError` 抛给登录流程。
async fn show_auth_prompt_ui(
    dialog: &Rc<RefCell<LoginDialogComponent>>,
    request: AuthPromptRequest,
    container: &Rc<RefCell<Container>>,
    renderer: &Rc<RefCell<crate::tui_renderer::InteractiveRenderer>>,
    provider_id: &str,
) -> Result<String, kiana_ai::auth::types::AuthError> {
    // 已经中止的提示不再展示，避免输入框一闪而过。
    if let Some(signal) = &request.signal
        && signal.is_aborted()
    {
        return Err(kiana_ai::auth::types::AuthError::new("Login cancelled"));
    }

    let future: std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<String, kiana_ai::auth::types::AuthError>> + '_,
        >,
    > = match &request.prompt {
        AuthPrompt::Select { message, options } => {
            let future =
                show_auth_select(dialog, message, options, container, renderer, provider_id);
            Box::pin(future)
        }
        AuthPrompt::ManualCode { message, .. } => {
            let receiver = dialog.borrow_mut().show_manual_input(message);
            Box::pin(await_dialog_receiver(receiver))
        }
        AuthPrompt::Text {
            message,
            placeholder,
        }
        | AuthPrompt::Secret {
            message,
            placeholder,
        } => {
            let receiver = dialog
                .borrow_mut()
                .show_prompt(message, placeholder.as_deref());
            Box::pin(await_dialog_receiver(receiver))
        }
    };

    // 单次提示的取消可能先于用户输入：丢弃输入 future 并按取消返回；没有 signal 时
    // 只能等输入/对话框取消。
    match &request.signal {
        None => future.await,
        Some(signal) => {
            tokio::select! {
                result = future => result,
                _ = signal.cancelled() => Err(kiana_ai::auth::types::AuthError::new("Login cancelled")),
            }
        }
    }
}

/// 等待对话框的输入结果。
///
/// 只有 `Ok(Ok(value))` 算成功；对话框取消（`LoginCancelledError`）与 sender 丢失（对话框
/// 被销毁）都映射为 `"Login cancelled"`。UI 循环串行处理提示，同一时刻只有一个待答
/// 提示，因此不会出现两个 receiver 竞争。
async fn await_dialog_receiver(
    receiver: futures::channel::oneshot::Receiver<
        Result<String, crate::components::login_dialog::LoginCancelledError>,
    >,
) -> Result<String, kiana_ai::auth::types::AuthError> {
    match receiver.await {
        Ok(Ok(value)) => Ok(value),
        _ => Err(kiana_ai::auth::types::AuthError::new("Login cancelled")),
    }
}

/// `showAuthSelect(dialog, prompt, providerId)`。
///
/// 登录中途的选项提示：临时把 `ExtensionSelectorComponent` 挂到编辑器槽位，选择/取消后先
/// 把对话框挂回槽位，再通过 oneshot 返回 **option id**（不是 label）。label 找不到对应 id
/// 时按取消处理。provider 为 Radius 时选择器额外带 `RADIUS_LOGIN_INTRO` 说明。
async fn show_auth_select(
    dialog: &Rc<RefCell<LoginDialogComponent>>,
    message: &str,
    options: &[kiana_ai::auth::types::AuthSelectOption],
    container: &Rc<RefCell<Container>>,
    renderer: &Rc<RefCell<crate::tui_renderer::InteractiveRenderer>>,
    provider_id: &str,
) -> Result<String, kiana_ai::auth::types::AuthError> {
    let (sender, receiver) = tokio::sync::oneshot::channel::<Option<String>>();
    let labels: Vec<String> = options.iter().map(|option| option.label.clone()).collect();
    let options_by_label: Vec<(String, String)> = options
        .iter()
        .map(|option| (option.label.clone(), option.id.clone()))
        .collect();
    let dialog_restore: kiana_tui::layout_node::SharedComponent = dialog.clone();
    let container_restore = Rc::clone(container);
    let renderer_restore = Rc::clone(renderer);
    let sender_select = Rc::new(RefCell::new(Some(sender)));
    // 选择与取消共享这个槽：先 take() 再发送，保证只应答一次。
    let sender_cancel = Rc::clone(&sender_select);
    let on_select = move |label: &str| {
        // 恢复对话框，然后返回选中项的 id。
        mount_shared_component(
            &container_restore,
            &renderer_restore,
            Rc::clone(&dialog_restore),
        );
        let id = options_by_label
            .iter()
            .find(|(option_label, _)| option_label == label)
            .map(|(_, id)| id.clone());
        if let Some(sender) = sender_select.borrow_mut().take() {
            let _ = sender.send(id);
        }
    };
    let dialog_cancel: kiana_tui::layout_node::SharedComponent = dialog.clone();
    let container_cancel = Rc::clone(container);
    let renderer_cancel = Rc::clone(renderer);
    let on_cancel = move || {
        mount_shared_component(
            &container_cancel,
            &renderer_cancel,
            Rc::clone(&dialog_cancel),
        );
        if let Some(sender) = sender_cancel.borrow_mut().take() {
            let _ = sender.send(None);
        }
    };
    let selector = ExtensionSelectorComponent::new(
        message,
        labels,
        on_select,
        on_cancel,
        ExtensionSelectorOptions {
            description: (provider_id == kiana_config::defaults::RADIUS_PROVIDER_ID)
                .then(|| crate::interactive_mode::RADIUS_LOGIN_INTRO.to_string()),
            ..Default::default()
        },
    );
    let shared = shared_component(selector);
    // 选择器临时占用编辑器槽位；on_select / on_cancel 负责把对话框挂回去。
    mount_shared_component(container, renderer, Rc::clone(&shared));
    // 取消与 label 未命中都发送 None → 统一按 “Login cancelled” 返回。
    match receiver.await {
        Ok(Some(id)) => Ok(id),
        _ => Err(kiana_ai::auth::types::AuthError::new("Login cancelled")),
    }
}
