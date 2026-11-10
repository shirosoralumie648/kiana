//! components/first-time-setup.ts。
//!
//! 首次启动设置：主题选择 + 匿名用量数据分享选择。**不**包含宿主决策逻辑（在命令层）。
//!
//! 每次变化都重建整个子树（主题预览要重着色所有文本）；选择移动 **clamp** 不环绕
//! （到边界即停）；默认主题 `system`、默认高亮 `true`（分享）。
//!
//! ## 状态机
//!
//! 组件只有一个两状态的私有枚举 `Step`：
//! 1. `Theme`（初始）：↑/↓（或字面 `k`/`j`）移动主题高亮，下标 clamp 在 `0..=2`；
//!    确认键（`tui.select.confirm`，测试用 `\r`）或字面 `\n` 进入 `Analytics`，
//!    此时不提交、也不改变已选主题。
//! 2. `Analytics`：↑/↓（或 `k`/`j`）移动分享选项，下标 clamp 在 `0..=1`；确认键/`\n`
//!    构造 [`FirstTimeSetupResult`] 并调用 `on_submit`，状态不再变化。
//!
//! 任意状态按取消键（`tui.select.cancel`，测试用 `\x1b`）都会调用 `on_cancel`，不产生结果。
//! 三个回调都返回 `()`，组件内没有错误分支；回调失败后如何退出由调用方决定。
//!
//! ## 回调与重建
//!
//! `FirstTimeSetupOptions` 的三个闭包由组件长期持有（`Box<dyn FnMut>`），在
//! `handle_input` 中同步调用。主题高亮**真正变化**时才调用 `on_theme_preview`；
//! 每次移动、进入下一步都调用 `update()` 清空并重建 `Container` 子树，因为主题预览要求
//! 所有文本按新主题重新着色。
//!
//! 调用方是 `kiana-coding-agent` 的启动 UI：`show_first_time_setup` 把组件挂到 TUI 根节点，
//! 通过一次性通道把「提交 `Some(result)` / 取消 `None`」送回驱动循环；只有提交才写设置。
//!
//! ## 注意事项
//!
//! - 选项文案、logo、analytics 段落都是 TS 的**逐字**字符串，改字会破坏 golden 对拍。
//! - 主题名取自 `theme_options()[theme_index]`（`system` / `dark` / `light`），不是界面上
//!   的高亮标签文本；`system` 用 `SYSTEM_THEME_NAME` 常量。
//! - `analytics_index = 0` 对应 `(true, "Share anonymous usage data")`，因此默认分享。

use std::any::Any;

use kiana_core::config::APP_NAME;
use kiana_theme::{SYSTEM_THEME_NAME, ThemeColor};
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::keybindings::get_keybindings;
use kiana_tui::tui::{Component, Container};
use kiana_ui_kit::dynamic_border::DynamicBorder;
use kiana_ui_kit::keybinding_hints::{key_hint, raw_key_hint};

fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

fn bold(text: &str) -> String {
    kiana_theme::THEME
        .bold(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// `FirstTimeSetupResult`：用户在两步对话框里最终确认的选择。
///
/// 该类型只承载 UI 选择，不做持久化；调用方拿到后自行写设置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstTimeSetupResult {
    /// 主题名，取自 `theme_options()` 的第一列：`"system"` / `"dark"` / `"light"`。
    pub theme: String,
    /// 是否分享匿名用量数据；`true` 对应第一项
    /// `"Share anonymous usage data"`，`false` 对应 `"Don't share"`。
    pub share_analytics: bool,
}

/// `FirstTimeSetupOptions`：构造对话框时注入的三个回调。
///
/// 回调由组件持有并在 `handle_input` 中同步执行；组件不会捕获 panic，也不会在回调返回后
/// 再校验状态。每个回调都可能被多次调用（`on_theme_preview` 每次真正移动都调用一次），
/// 因此实现方需要自行处理重复调用。
pub struct FirstTimeSetupOptions {
    /// 主题高亮移动时预览（必填）。参数是新的主题名（`"system"` / `"dark"` / `"light"`），
    /// 仅在 `theme_index` 实际发生变化时调用。
    pub on_theme_preview: Box<dyn FnMut(&str)>,
    /// 提交结果：只在 `Analytics` 步骤按下确认键时调用，参数是最终选择。
    pub on_submit: Box<dyn FnMut(FirstTimeSetupResult)>,
    /// 取消（跳过设置）：任意步骤按下取消键时调用，无参数。
    pub on_cancel: Box<dyn FnMut()>,
}

/// `THEME_OPTIONS`（**【逐字】**）。
fn theme_options() -> [(String, &'static str); 3] {
    [
        (
            SYSTEM_THEME_NAME.to_string(),
            "System (matches your terminal colors)",
        ),
        ("dark".to_string(), "Dark"),
        ("light".to_string(), "Light"),
    ]
}

/// `ANALYTICS_OPTIONS`（**【逐字】**）。
const ANALYTICS_OPTIONS: [(bool, &str); 2] =
    [(true, "Share anonymous usage data"), (false, "Don't share")];

/// `SETUP_LOGO_LINES`。
const SETUP_LOGO_LINES: [&str; 4] = ["██████", "██  ██", "████  ██", "██    ██"];

/// 四行段落（单个 `Text`，`\n` 分隔；**【逐字】**）。
const ANALYTICS_PARAGRAPH: &str = "Opting in stores a tracking identifier in settings.json and enables anonymous\nusage analytics. This helps us to better debug, reproduce, and resolve issues\nand bugs within Kiana. You can observe what is shared using /privacy and make\nchanges anytime in settings.json.";

/// 对话框所处的步骤；只前进不后退（确认后 `Theme → Analytics`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// 主题选择，界面显示 3 个选项，`theme_index` 有效。
    Theme,
    /// 匿名用量选择，界面显示 2 个选项，`analytics_index` 有效。
    Analytics,
}

/// 首次启动设置对话框（TS `class FirstTimeSetupComponent extends Container`）。
///
/// 组合而非继承：内部持有一个 `Container` 子树，`render` 直接转发给它。`update()` 会在
/// 每次状态变化后清空并重建子树，所以 `container` 里的组件实例每帧都可能是新的。
/// 选择状态保存在本结构体（`step` / `theme_index` / `analytics_index`），重建不会丢失。
pub struct FirstTimeSetupComponent {
    container: Container,
    step: Step,
    theme_index: usize,
    analytics_index: usize,
    options: FirstTimeSetupOptions,
}

impl FirstTimeSetupComponent {
    /// TS `constructor(options)`。
    ///
    /// 初始状态为 `Theme`、`theme_index = 0`（`system`）、`analytics_index = 0`（分享）。
    /// 构造末尾立即调用一次 `update()`，因此 `new` 返回后无需额外渲染即可拿到完整对话框。
    pub fn new(options: FirstTimeSetupOptions) -> Self {
        let mut component = FirstTimeSetupComponent {
            container: Container::new(),
            step: Step::Theme,
            theme_index: 0,
            analytics_index: 0,
            options,
        };
        component.update();
        component
    }

    /// 当前步骤（`"theme"` / `"analytics"`；当前仓库内无调用方，仅供内省）。
    pub fn step_name(&self) -> &'static str {
        match self.step {
            Step::Theme => "theme",
            Step::Analytics => "analytics",
        }
    }

    /// 当前主题下标（`0..=2`，对应 `theme_options()` 的三项）。
    pub fn theme_index(&self) -> usize {
        self.theme_index
    }

    /// 当前 analytics 下标（`0` = 分享，`1` = 不分享）。
    pub fn analytics_index(&self) -> usize {
        self.analytics_index
    }

    /// 重建整个子树：外框 → logo → 欢迎语 → 当前步骤的内容 → 提示行 → 外框。
    ///
    /// 每次调用先 `clear()`，因此不会出现旧步骤的残留组件。选中项用 `→ ` 前缀和 Accent
    /// 颜色标记；未选中项用两个空格对齐。主题预览要求整棵树重新着色，所以这里选择整树
    /// 重建而不是局部更新。
    fn update(&mut self) {
        self.container.clear();
        self.container.add_child(Box::new(DynamicBorder::default()));
        self.container.add_child(Box::new(Spacer::new()));
        self.container.add_child(Box::new(Text::with_padding(
            fg(ThemeColor::Accent, &SETUP_LOGO_LINES.join("\n")),
            1,
            0,
        )));
        self.container.add_child(Box::new(Spacer::new()));
        self.container.add_child(Box::new(Text::with_padding(
            fg(
                ThemeColor::Accent,
                &bold(&format!("Welcome to {APP_NAME}, the minimal coding agent.")),
            ),
            1,
            0,
        )));
        self.container.add_child(Box::new(Spacer::new()));

        match self.step {
            Step::Theme => {
                self.container.add_child(Box::new(Text::with_padding(
                    fg(ThemeColor::Text, "Pick a theme."),
                    1,
                    0,
                )));
                self.container.add_child(Box::new(Spacer::new()));
                let labels: Vec<String> = theme_options()
                    .iter()
                    .map(|(_, label)| (*label).to_string())
                    .collect();
                self.add_option_list(&labels, self.theme_index);
            }
            Step::Analytics => {
                self.container.add_child(Box::new(Text::with_padding(
                    fg(ThemeColor::Text, "Opt-in to anonymous usage data sharing?"),
                    1,
                    0,
                )));
                self.container.add_child(Box::new(Text::with_padding(
                    fg(ThemeColor::Muted, ANALYTICS_PARAGRAPH),
                    1,
                    0,
                )));
                self.container.add_child(Box::new(Spacer::new()));
                let labels: Vec<String> = ANALYTICS_OPTIONS
                    .iter()
                    .map(|(_, label)| (*label).to_string())
                    .collect();
                self.add_option_list(&labels, self.analytics_index);
            }
        }

        self.container.add_child(Box::new(Spacer::new()));
        let hint = raw_key_hint("↑↓", "navigate")
            + "  "
            + &key_hint(
                "tui.select.confirm",
                if self.step == Step::Theme {
                    "continue"
                } else {
                    "finish"
                },
            )
            + "  "
            + &key_hint("tui.select.cancel", "skip setup");
        self.container
            .add_child(Box::new(Text::with_padding(hint, 1, 0)));
        self.container.add_child(Box::new(Spacer::new()));
        self.container.add_child(Box::new(DynamicBorder::default()));
    }

    /// 把一组标签按顺序加为单行文本，`selected_index` 那一项高亮。
    ///
    /// 所有选项用相同的左右内边距（1 列），高亮项多出的 `→ ` 前缀由未选中项的等宽空格
    /// 前缀抵消，因此切换高亮不会让文字左右跳动。
    fn add_option_list(&mut self, labels: &[String], selected_index: usize) {
        for (index, label) in labels.iter().enumerate() {
            let is_selected = index == selected_index;
            let prefix = if is_selected {
                fg(ThemeColor::Accent, "→ ")
            } else {
                "  ".to_string()
            };
            let label = if is_selected {
                fg(ThemeColor::Accent, label)
            } else {
                fg(ThemeColor::Text, label)
            };
            self.container.add_child(Box::new(Text::with_padding(
                format!("{prefix}{label}"),
                1,
                0,
            )));
        }
    }

    /// 按 `delta`（当前只传入 -1/+1）移动当前步骤的高亮项。
    ///
    /// clamp 而非环绕：在首项继续向上、在末项继续向下都停在原地。主题项只有下标真的
    /// 变化时才触发 `on_theme_preview` 并重建；analytics 项不触发预览。无论是否移动，
    /// 末尾都会 `update()`（clamp 后重绘的结果与之前相同）。
    fn move_selection(&mut self, delta: i64) {
        match self.step {
            Step::Theme => {
                let next = (self.theme_index as i64 + delta).clamp(0, 2) as usize;
                if next != self.theme_index {
                    self.theme_index = next;
                    let preview = theme_options()[self.theme_index].0.clone();
                    (self.options.on_theme_preview)(&preview);
                }
            }
            Step::Analytics => {
                self.analytics_index = (self.analytics_index as i64 + delta).clamp(0, 1) as usize;
            }
        }
        self.update();
    }
}

impl Component for FirstTimeSetupComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&mut self, key_data: &str) {
        let kb = get_keybindings();
        // 方向键先看可配置键位绑定，再接受 vim 风格的字面 `k`/`j`（测试直接发 "j"）。
        if kb.matches(key_data, "tui.select.up") || key_data == "k" {
            self.move_selection(-1);
        } else if kb.matches(key_data, "tui.select.down") || key_data == "j" {
            self.move_selection(1);
        } else if kb.matches(key_data, "tui.select.confirm") || key_data == "\n" {
            // 确认键在两个步骤含义不同：Theme 进下一步，Analytics 才真正提交。
            if self.step == Step::Theme {
                self.step = Step::Analytics;
                self.update();
            } else {
                let result = FirstTimeSetupResult {
                    theme: theme_options()[self.theme_index].0.clone(),
                    share_analytics: ANALYTICS_OPTIONS[self.analytics_index].0,
                };
                (self.options.on_submit)(result);
            }
        } else if kb.matches(key_data, "tui.select.cancel") {
            // 只通知调用方跳过设置；本组件不重置状态，也不自行从父容器移除。
            (self.options.on_cancel)();
        }
    }

    fn handles_input(&self) -> bool {
        true
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

    /// TS：`invalidate() { this.update(); super.invalidate(); }`（**先重建再 super**）。
    fn invalidate(&mut self) {
        self.update();
        self.container.invalidate();
    }
}
