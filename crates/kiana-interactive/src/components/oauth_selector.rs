//! components/oauth-selector.ts。
//!
//! `/login`、`/logout` 的 provider 选择器：搜索、状态后缀、认证类型标签。
//!
//! up/down **clamp**（不环绕）；选项由宿主投影，组件**不做**任何 provider 特定过滤。
//!
//! ## 职责与调用方
//!
//! `auth.rs` 的登录/登出流程用 [`OAuthSelectorComponent::new`] 构造本组件并挂到 UI 槽位。
//! 选择时通过回调把 `(provider id, auth_type)` 交回调用方，取消时调用 `on_cancel`。
//! 组件不访问网络或凭据存储：选项、认证状态与订阅标志都由宿主投影成
//! [`AuthSelectorProvider`]。
//!
//! ## 交互模型
//!
//! * 搜索框是唯一的文本输入。导航/确认/取消键先由 `handle_input` 解释，未命中的按键才交给
//!   搜索框并重新过滤；
//! * `up` / `down` 在结果集两端 clamp（不环绕），结果集为空时直接忽略；
//! * 过滤使用 `kiana_tui::fuzzy::fuzzy_filter`，匹配文本由 `name + id + auth_type +
//!   method.name` 拼成，因此可以按名称、id 或认证方式搜索；
//! * 列表最多显示 8 行，选中项尽量居中；窗口未覆盖全部结果时追加 `(当前/总数)` 提示。
//!
//! ## 渲染约定
//!
//! 状态后缀由 [`format_auth_selector_provider_status`] 生成，认证类型标签由
//! [`format_auth_selector_provider_type`] 生成。只有选项中出现不止一种 `auth_type` 时，
//! 才显示 `[API key]` / `[subscription]` 标签。

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::LazyLock;

use kiana_ai::auth::types::{ApiKeyAuth, AuthCheck, AuthType, OAuthAuth};
use kiana_theme::ThemeColor;
use kiana_tui::components::input::Input;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::truncated_text::TruncatedText;
use kiana_tui::fuzzy::fuzzy_filter;
use kiana_tui::keybindings::get_keybindings;
use kiana_tui::tui::{Component, Container, Focusable, TuiMouseEvent, TuiMouseEventResult};
use kiana_ui_kit::dynamic_border::DynamicBorder;

/// 按全局主题着色（TS `theme.fg(token, str)`）。与 `login_dialog.rs` 的同名函数一致：
/// 主题未初始化时 panic，因为 TS 渲染路径同样会在此时抛错。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// TS `theme.bold(text)`。
fn bold(text: &str) -> String {
    kiana_theme::THEME
        .bold(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// 选择器的两种模式。
///
/// 模式只影响标题与“无可选 provider”时的提示文本，不改变过滤或选择逻辑。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthSelectorMode {
    /// `/login`。
    Login,
    /// `/logout`。
    Logout,
}

/// provider 的 auth 方式（TS `ApiKeyAuth | OAuthAuth`；唯一被组件读取的属性是 `name`）。
///
/// 组件只用它拼搜索文本，不调用任何认证行为；`Arc` 允许宿主与组件共享同一实例。
#[derive(Clone)]
pub enum AuthSelectorMethod {
    /// API key auth。
    ApiKey(Arc<dyn ApiKeyAuth>),
    /// OAuth auth。
    OAuth(Arc<dyn OAuthAuth>),
}

impl AuthSelectorMethod {
    /// `method?.name`。
    pub fn name(&self) -> &str {
        match self {
            AuthSelectorMethod::ApiKey(auth) => auth.name(),
            AuthSelectorMethod::OAuth(auth) => auth.name(),
        }
    }
}

/// `AuthSelectorProvider`：宿主对 provider 的投影。
///
/// 选择回调收到的是 `id` 与 `auth_type`，不是展示名；`status` 是宿主预先检测好的认证
/// 状态，组件只负责格式化显示，不会重新检测。
#[derive(Clone)]
pub struct AuthSelectorProvider {
    /// provider id。
    pub id: String,
    /// 展示名。
    pub name: String,
    /// 该选项使用的 auth 类型。
    pub auth_type: AuthType,
    /// auth 方式（仅用于搜索文本里的 `method.name`）。
    pub method: Option<AuthSelectorMethod>,
    /// 编译后的 auth 状态。
    pub status: Option<AuthCheck>,
    /// OAuth 是否由订阅支撑（`false` → `account` 标签）。
    pub subscription: Option<bool>,
}

/// `formatAuthSelectorProviderType(authType, subscription?)`。
///
/// 映射：`ApiKey` → `"API key"`；其余（`OAuth`）在 `subscription == Some(false)` 时为
/// `"account"`，否则为 `"subscription"`。`subscription` 对 API key 无影响。
pub fn format_auth_selector_provider_type(
    auth_type: AuthType,
    subscription: Option<bool>,
) -> String {
    if auth_type == AuthType::ApiKey {
        return "API key".to_string();
    }
    if subscription == Some(false) {
        "account".to_string()
    } else {
        "subscription".to_string()
    }
}

/// 环境变量名单（`/^[A-Z][A-Z0-9_]*(?:, [A-Z][A-Z0-9_]*)*$/`）。
static ENV_SOURCE_PATTERN: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[A-Z][A-Z0-9_]*(?:, [A-Z][A-Z0-9_]*)*$").expect("valid env regex")
});

/// `formatAuthSelectorProviderStatus(provider)`。
///
/// 返回带前导 `" • "` / `" ✓ "` 的状态后缀（颜色已用 ANSI 写死在字符串里）：
///
/// 1. `status` 为 `None` → Muted 的 `" • not configured"`；
/// 2. `status.r#type` 与 `provider.auth_type` 不同 → Muted 的 `" • "` 后接 Warning 的
///    `"<类型> configured"`，用于提示“配置的是另一种认证方式”；
/// 3. 类型一致时：来源为空 / `"OAuth"` / `"stored credential"` → `" ✓ configured"`；
///    形如环境变量列表（正则见 `ENV_SOURCE_PATTERN`）→ `" ✓ env: <来源>"`；其它来源
///    原样显示为 `" ✓ <来源>"`。这几种都使用 Success 颜色。
pub fn format_auth_selector_provider_status(provider: &AuthSelectorProvider) -> String {
    let Some(status) = &provider.status else {
        return fg(ThemeColor::Muted, " • not configured");
    };
    if status.r#type != provider.auth_type {
        let label = format!(
            "{} configured",
            format_auth_selector_provider_type(status.r#type, provider.subscription)
        );
        return fg(ThemeColor::Muted, " • ") + &fg(ThemeColor::Warning, &label);
    }
    let source = status.source.as_deref().unwrap_or("");
    if source.is_empty() || source == "OAuth" || source == "stored credential" {
        return fg(ThemeColor::Success, " ✓ configured");
    }
    let source = if ENV_SOURCE_PATTERN.is_match(source) {
        format!("env: {source}")
    } else {
        source.to_string()
    };
    fg(ThemeColor::Success, &format!(" ✓ {source}"))
}

/// 搜索输入代理。
struct InputProxy(Rc<RefCell<Input>>);

impl Component for InputProxy {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.borrow_mut().handle_mouse(event)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for InputProxy {
    fn focused(&self) -> bool {
        self.0.borrow().focused()
    }

    fn set_focused(&mut self, focused: bool) {
        self.0.borrow_mut().set_focused(focused);
    }
}

/// 列表容器代理。
struct ListProxy(Rc<RefCell<Container>>);

impl Component for ListProxy {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.borrow_mut().handle_mouse(event)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
}

/// 选择回调（TS `onSelect: (providerId, authType) => void`）。
pub type AuthSelectorSelectCallback = Box<dyn FnMut(&str, AuthType)>;

/// 授权 provider 选择器（TS `class OAuthSelectorComponent extends Container implements Focusable`）。
///
/// 固定外壳（边框、标题、搜索框、列表容器）在构造时一次搭好；过滤与导航只重建列表容器的
/// 子项。`selected_index` 始终指向 `filtered_providers` 的合法下标；结果集为空时保持 0，
/// 此时选择操作是空操作。
pub struct OAuthSelectorComponent {
    container: Container,
    search_input: Rc<RefCell<Input>>,
    list_container: Rc<RefCell<Container>>,
    all_providers: Vec<AuthSelectorProvider>,
    filtered_providers: Vec<AuthSelectorProvider>,
    selected_index: usize,
    mode: AuthSelectorMode,
    on_select: AuthSelectorSelectCallback,
    on_cancel: Box<dyn FnMut()>,
    show_auth_type_labels: bool,
    focused: bool,
}

impl OAuthSelectorComponent {
    /// TS `constructor(mode, providers, onSelect, onCancel, initialSearchInput?)`。
    ///
    /// `show_auth_type_labels` 在构造时按“出现过的不同 `auth_type` 数 > 1”计算并固定；
    /// 传入 `initial_search_input` 时先写入搜索框，再以其内容执行一次过滤（因此初始列表
    /// 可能已被裁剪）。
    pub fn new(
        mode: AuthSelectorMode,
        providers: Vec<AuthSelectorProvider>,
        on_select: impl FnMut(&str, AuthType) + 'static,
        on_cancel: impl FnMut() + 'static,
        initial_search_input: Option<&str>,
    ) -> Self {
        let show_auth_type_labels = providers
            .iter()
            .map(|provider| provider.auth_type)
            .collect::<std::collections::HashSet<_>>()
            .len()
            > 1;
        let filtered_providers = providers.clone();

        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorder::default()));
        container.add_child(Box::new(Spacer::new()));

        let title = match mode {
            AuthSelectorMode::Login => "Select provider to configure:",
            AuthSelectorMode::Logout => "Select provider to logout:",
        };
        container.add_child(Box::new(TruncatedText::with_padding(
            fg(ThemeColor::Accent, &bold(title)),
            1,
            0,
        )));
        container.add_child(Box::new(Spacer::new()));

        let search_input = Rc::new(RefCell::new(Input::new(Default::default())));
        if let Some(initial_search_input) = initial_search_input {
            search_input.borrow_mut().set_value(initial_search_input);
        }
        container.add_child(Box::new(InputProxy(Rc::clone(&search_input))));
        container.add_child(Box::new(Spacer::new()));

        let list_container = Rc::new(RefCell::new(Container::new()));
        container.add_child(Box::new(ListProxy(Rc::clone(&list_container))));
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(DynamicBorder::default()));

        let mut selector = OAuthSelectorComponent {
            container,
            search_input,
            list_container,
            all_providers: providers,
            filtered_providers,
            selected_index: 0,
            mode,
            on_select: Box::new(on_select),
            on_cancel: Box::new(on_cancel),
            show_auth_type_labels,
            focused: false,
        };
        selector.filter_providers(initial_search_input.unwrap_or(""));
        selector
    }

    /// 当前过滤结果（只读；测试用）。
    ///
    /// 顺序与 `all_providers` 中通过过滤的相对顺序一致；空查询时就是全部 provider。
    pub fn filtered_providers(&self) -> &[AuthSelectorProvider] {
        &self.filtered_providers
    }

    /// 当前选中下标。
    ///
    /// 已按当前过滤结果 clamp；结果集为空时为 0（此时没有任何项被选中）。
    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    /// 当前搜索文本（副本）。
    pub fn search_value(&self) -> String {
        self.search_input.borrow().get_value().to_string()
    }

    fn filter_providers(&mut self, query: &str) {
        // 空查询直接恢复完整列表（绕过 fuzzy 匹配），否则按拼好的搜索文本做模糊过滤。
        // 过滤后把选中下标 clamp 到新长度内，避免结果变短后指向不存在的条目。
        self.filtered_providers = if query.is_empty() {
            self.all_providers.clone()
        } else {
            fuzzy_filter(self.all_providers.clone(), query, |provider| {
                format!(
                    "{} {} {} {}",
                    provider.name,
                    provider.id,
                    provider.auth_type.as_str(),
                    provider
                        .method
                        .as_ref()
                        .map(AuthSelectorMethod::name)
                        .unwrap_or("")
                )
            })
        };
        let length = self.filtered_providers.len();
        let max_index = length.saturating_sub(1);
        self.selected_index = usize::min(self.selected_index, max_index);
        self.update_list();
    }

    fn update_list(&mut self) {
        let mut list = self.list_container.borrow_mut();
        list.clear();

        // 视窗算法：固定最多 8 行，尽量让选中项居中。
        // start = max(0, min(selected - 4, length - 8))，end = min(start + 8, length)；
        // 结果不足 8 行时 clamp 到 0，不会产生负下标。
        let max_visible: usize = 8;
        let length = self.filtered_providers.len() as i64;
        let selected = self.selected_index as i64;
        let start_index = i64::max(
            0,
            i64::min(
                selected - (max_visible as i64) / 2,
                length - max_visible as i64,
            ),
        );
        let end_index = i64::min(start_index + max_visible as i64, length);

        let mut index = start_index;
        while index < end_index {
            let Some(provider) = self.filtered_providers.get(index as usize) else {
                index += 1;
                continue;
            };
            let is_selected = index == selected;
            let status_indicator = format_auth_selector_provider_status(provider);
            let auth_type_label = if self.show_auth_type_labels {
                fg(
                    ThemeColor::Muted,
                    &format!(
                        " [{}]",
                        format_auth_selector_provider_type(
                            provider.auth_type,
                            provider.subscription
                        )
                    ),
                )
            } else {
                String::new()
            };
            let line = if is_selected {
                fg(ThemeColor::Accent, "→ ")
                    + &fg(ThemeColor::Accent, &provider.name)
                    + &auth_type_label
                    + &status_indicator
            } else {
                format!("  {}", fg(ThemeColor::Text, &provider.name))
                    + &auth_type_label
                    + &status_indicator
            };
            list.add_child(Box::new(TruncatedText::with_padding(line, 1, 0)));
            index += 1;
        }

        // 只有窗口没有覆盖全部结果时才显示滚动位置（1 基的当前项 / 总数）。
        if start_index > 0 || end_index < length {
            let scroll_info = fg(
                ThemeColor::Muted,
                &format!("  ({}/{})", self.selected_index + 1, length),
            );
            list.add_child(Box::new(TruncatedText::with_padding(scroll_info, 1, 0)));
        }

        // 空结果要区分两种原因：宿主没给任何 provider，还是当前搜索无匹配。
        if self.filtered_providers.is_empty() {
            let message = if self.all_providers.is_empty() {
                match self.mode {
                    AuthSelectorMode::Login => "No providers available",
                    AuthSelectorMode::Logout => "No providers logged in. Use /login first.",
                }
            } else {
                "No matching providers"
            };
            list.add_child(Box::new(TruncatedText::with_padding(
                fg(ThemeColor::Muted, &format!("  {message}")),
                1,
                0,
            )));
        }
    }

    fn select_current(&mut self) -> bool {
        // 空结果集或下标越界时是空操作（不调用回调）。先复制 id/auth_type 再调用回调，
        // 避免 `self` 的不可变借用与回调可能引发的再次借用冲突。
        let Some(provider) = self.filtered_providers.get(self.selected_index) else {
            return false;
        };
        let id = provider.id.clone();
        let auth_type = provider.auth_type;
        (self.on_select)(&id, auth_type);
        true
    }
}

impl Component for OAuthSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&mut self, key_data: &str) {
        let kb = get_keybindings();
        // 分支顺序：导航 > 确认 > 取消 > 搜索框提交 > 文本输入。前四类命中后不再把按键
        // 交给搜索框；只有最后的普通字符才进入搜索并触发重新过滤。
        if kb.matches(key_data, "tui.select.up") {
            if self.filtered_providers.is_empty() {
                return;
            }
            self.selected_index = self.selected_index.saturating_sub(1);
            self.update_list();
        } else if kb.matches(key_data, "tui.select.down") {
            if self.filtered_providers.is_empty() {
                return;
            }
            self.selected_index =
                usize::min(self.filtered_providers.len() - 1, self.selected_index + 1);
            self.update_list();
        } else if kb.matches(key_data, "tui.select.confirm") {
            self.select_current();
        } else if kb.matches(key_data, "tui.select.cancel") {
            (self.on_cancel)();
        } else if kb.matches(key_data, "tui.input.submit") || key_data == "\n" {
            // `searchInput.onSubmit`：提交时选择当前项（与 `tui.select.confirm` 同义）。
            self.select_current();
        } else {
            self.search_input.borrow_mut().handle_input(key_data);
            let query = self.search_input.borrow().get_value().to_string();
            self.filter_providers(&query);
        }
    }

    fn handles_input(&self) -> bool {
        true
    }

    /// TS `Container.handleMouse`：按行路由到搜索输入框。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.container.handle_mouse(event)
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.container.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.container.children)
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }
}

impl Focusable for OAuthSelectorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.search_input.borrow_mut().set_focused(focused);
    }
}
