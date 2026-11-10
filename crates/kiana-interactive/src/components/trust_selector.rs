//! components/trust-selector.ts。
//!
//! 项目信任选择器：显示已保存决定、当前会话状态与信任选项（含父目录选项）。
//!
//! up/down **clamp**（不环绕）；选中项 label 用 accent，未选中用 text。
//!
//! # 调用方与数据流
//!
//! 生产入口是 `interactive_mode/commands/auth.rs` 的 `show_trust_selector`（`/trust`
//! 命令）：宿主传入 cwd、`trust.json` 里该目录的最近决定（读取失败按"无决定"处理）与
//! 当前会话的信任状态。选项列表来自
//! [`get_project_trust_options`]（`kiana-config`），构造时以 `include_session_only =
//! false` 调用，因此列表固定为：`Trust` →（有父目录时）`Trust parent folder (<parent>)`
//! → `Do not trust`，不含两个 session-only 选项。
//!
//! 确认时组件只把选中项的 `trusted` + `updates` 交回宿主（[`TrustSelection`]，丢弃
//! label/savedPath）；**写盘由宿主完成**，组件不做任何持久化。`updates` 的语义见
//! `kiana-config` 的 trust manager：父目录选项是"先信任父目录、再删除 cwd 自己的键"，
//! 保存后 cwd 继承父目录的决定。
//!
//! # 渲染实现与 TS 的差异
//!
//! TS 里 `listContainer` 是普通子项；Rust 侧列表放在独立的 `ListProxy` 字段里，
//! [`Component::render`] 在容器子项序列的第 8 个位置手工插入它的输出（见 render 内
//! 注释）。键盘处理也由本组件自己实现（clamp 索引 + 重建列表），没有复用
//! `SelectList`——这是与 theme/thinking 选择器最大的结构差异。
//!
//! # 边界与失败模式
//!
//! - 初始选中 = 与 `saved_decision` 完全匹配的选项（`is_saved_option` 的唯一判据：
//!   `decision == option.trusted && decision.path == saved_path`），找不到则 0。
//! - `selected_index` 用 `i64` 是为了与 TS 的 `Math.max(0, ...)` 一致；空列表时 down
//!   会把索引压到 -1（TS 同样存在的边界），此时确认分支 `trust_options.get(-1)` 返回
//!   `None`，回调不触发。
//! - `k` / `j` / `"\n"` 是硬编码兜底键位，与 keybinding 匹配并列。

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use kiana_config::trust::manager::{
    ProjectTrustOption, ProjectTrustStoreEntry, ProjectTrustUpdate, get_project_trust_options,
};
use kiana_theme::ThemeColor;
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

/// `TrustSelection = Pick<ProjectTrustOption, "trusted" | "updates">`。
///
/// 确认时交给宿主的载荷：只保留决策本身，展示字段（label/savedPath）不出组件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustSelection {
    /// 是否信任。
    pub trusted: bool,
    /// 选择后写入的更新。
    pub updates: Vec<ProjectTrustUpdate>,
}

/// `TrustSelectorOptions`。
///
/// 生命周期注意：`on_select` / `on_cancel` 是 `Box<dyn FnMut>`，被组件独占持有；宿主
/// 侧通常在闭包里通过 `Weak` 升级回交互模式，组件销毁后升级失败即静默返回（见
/// `commands/auth.rs` 的 `show_trust_selector`）。
pub struct TrustSelectorOptions {
    /// 工作目录。
    pub cwd: String,
    /// 已保存的最近决定（`None` == TS 的 `null`）。
    pub saved_decision: Option<ProjectTrustStoreEntry>,
    /// 当前会话是否已信任。
    pub project_trusted: bool,
    /// 选择回调。
    pub on_select: Box<dyn FnMut(TrustSelection)>,
    /// 取消回调。
    pub on_cancel: Box<dyn FnMut()>,
}

/// TS 私有 `formatDecision(trustPath, decision)`。
///
/// "Saved decision:" 行的文案。`trust_path` 实参固定为第一个选项的 `saved_path`（即
/// `normalizeCwd(cwd)`，见构造函数），不是逐项计算；决策的 `path` 与它不同时说明该决定
/// 来自祖先目录，显示为 `inherited from`。
fn format_decision(trust_path: Option<&str>, decision: Option<&ProjectTrustStoreEntry>) -> String {
    let Some(decision) = decision else {
        return "none".to_string();
    };
    let label = if decision.decision {
        "trusted"
    } else {
        "untrusted"
    };
    if let Some(trust_path) = trust_path
        && decision.path != trust_path
    {
        return format!("{label} (inherited from {})", decision.path);
    }
    format!("{label} ({})", decision.path)
}

/// 信任选择器（TS `class TrustSelectorComponent extends Container`）。
pub struct TrustSelectorComponent {
    container: Container,
    list_container: Rc<RefCell<Container>>,
    selected_index: i64,
    trust_options: Vec<ProjectTrustOption>,
    saved_decision: Option<ProjectTrustStoreEntry>,
    on_select: Box<dyn FnMut(TrustSelection)>,
    on_cancel: Box<dyn FnMut()>,
}

impl TrustSelectorComponent {
    /// TS `constructor(options)`。
    ///
    /// 构造期一次性读取选项并定位初始选中项（`is_saved_option` 命中项，否则 0），
    /// 之后不再查询信任库——"Saved decision:" 是打开瞬间的快照。布局子项顺序固定
    /// （见模块文档与 spec §2.54.9，改动需同步黄金渲染测试）；列表占容器第 9 个子项
    /// （渲染下标 8）。
    pub fn new(options: TrustSelectorOptions) -> Self {
        let trust_options = get_project_trust_options(&options.cwd, false);
        let saved_decision = options.saved_decision;
        let initial_index = trust_options
            .iter()
            .position(|option| is_saved_option(option, saved_decision.as_ref()))
            .map(|index| index as i64)
            .unwrap_or(-1);
        let selected_index = i64::max(0, initial_index);

        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorder::default()));
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(Text::with_padding(
            fg(ThemeColor::Accent, &bold("Project trust")),
            1,
            0,
        )));
        container.add_child(Box::new(Text::with_padding(
            fg(ThemeColor::Muted, &options.cwd),
            1,
            0,
        )));
        container.add_child(Box::new(Spacer::new()));
        let trust_path = trust_options
            .first()
            .and_then(|option| option.saved_path.clone());
        container.add_child(Box::new(Text::with_padding(
            fg(
                ThemeColor::Muted,
                &format!(
                    "Saved decision: {}",
                    format_decision(trust_path.as_deref(), saved_decision.as_ref())
                ),
            ),
            1,
            0,
        )));
        container.add_child(Box::new(Text::with_padding(
            fg(
                ThemeColor::Muted,
                &format!(
                    "Current session: {}",
                    if options.project_trusted {
                        "trusted"
                    } else {
                        "untrusted"
                    }
                ),
            ),
            1,
            0,
        )));
        container.add_child(Box::new(Spacer::new()));

        let list_container = Rc::new(RefCell::new(Container::new()));
        container.add_child(Box::new(ListProxy(Rc::clone(&list_container))));
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(Text::with_padding(
            raw_key_hint("↑↓", "navigate")
                + "  "
                + &key_hint("tui.select.confirm", "save")
                + "  "
                + &key_hint("tui.select.cancel", "cancel"),
            1,
            0,
        )));
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(DynamicBorder::default()));

        let mut selector = TrustSelectorComponent {
            container,
            list_container,
            selected_index,
            trust_options,
            saved_decision,
            on_select: options.on_select,
            on_cancel: options.on_cancel,
        };
        selector.update_list();
        selector
    }

    /// 当前选中下标。
    pub fn selected_index(&self) -> i64 {
        self.selected_index
    }

    fn update_list(&mut self) {
        // 每次移动都 clear + 全量重建：行数最多 3 条，重建比局部改写 Text 简单，且与
        // TS 的 `updateList()`（clear + 循环 addChild）逐行为对应。
        self.list_container.borrow_mut().clear();
        for (index, option) in self.trust_options.iter().enumerate() {
            let is_selected = index as i64 == self.selected_index;
            let is_current = is_saved_option(option, self.saved_decision.as_ref());
            let current_marker = if is_current {
                fg(ThemeColor::Accent, "✓ ")
            } else {
                "  ".to_string()
            };
            let prefix = if is_selected {
                fg(ThemeColor::Accent, "→ ")
            } else {
                "  ".to_string()
            };
            let label = if is_selected {
                fg(ThemeColor::Accent, &option.label)
            } else {
                fg(ThemeColor::Text, &option.label)
            };
            self.list_container
                .borrow_mut()
                .add_child(Box::new(Text::with_padding(
                    format!("{prefix}{current_marker}{label}"),
                    1,
                    0,
                )));
        }
    }
}

fn is_saved_option(
    option: &ProjectTrustOption,
    saved_decision: Option<&ProjectTrustStoreEntry>,
) -> bool {
    // 「当前」标记（✓）的唯一判据：option 有 saved_path 且决策的 decision/path 与它
    // 完全一致。不比较 project_trusted，也不识别 session-only 选项（本组件不启用）。
    let Some(saved_path) = &option.saved_path else {
        return false;
    };
    let Some(decision) = saved_decision else {
        return false;
    };
    decision.decision == option.trusted && decision.path == *saved_path
}

/// 列表容器代理。
struct ListProxy(Rc<RefCell<Container>>);

impl Component for ListProxy {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
}

impl Component for TrustSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        // 容器顺序：… → [list_container]（下标 8）→ …（列表是独立字段，显式插入）。
        // TS 的 listContainer 是普通子项；这里因为列表要随键盘状态整体重建（clear +
        // 重新 add），放在独立字段而不是容器子项里，渲染时在固定下标处拼接输出。下标
        // 8 对应构造函数里 add_child 的第 9 个子项（DynamicBorder + 2×Spacer + 4×Text
        // + Spacer 之后）。
        let mut lines = Vec::new();
        for (index, child) in self.container.children.iter_mut().enumerate() {
            if index == 8 {
                lines.extend(self.list_container.borrow_mut().render(width));
                continue;
            }
            lines.extend(child.render(width));
        }
        lines
    }

    fn handle_input(&mut self, key_data: &str) {
        // 键位处理（clamp，不环绕；k/j/\n 是硬编码兜底）：每次移动后整体重建列表，
        // 选中项标 `→ `、saved 决策标 `✓`，其余行保持原样。确认只回传 trusted +
        // updates；索引越界（如空列表压到 -1）时 get 返回 None，静默不触发。
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.select.up") || key_data == "k" {
            self.selected_index = i64::max(0, self.selected_index - 1);
            self.update_list();
        } else if kb.matches(key_data, "tui.select.down") || key_data == "j" {
            self.selected_index =
                i64::min(self.trust_options.len() as i64 - 1, self.selected_index + 1);
            self.update_list();
        } else if kb.matches(key_data, "tui.select.confirm") || key_data == "\n" {
            let selected = usize::try_from(self.selected_index)
                .ok()
                .and_then(|index| self.trust_options.get(index));
            if let Some(selected) = selected {
                let selection = TrustSelection {
                    trusted: selected.trusted,
                    updates: selected.updates.clone(),
                };
                (self.on_select)(selection);
            }
        } else if kb.matches(key_data, "tui.select.cancel") {
            (self.on_cancel)();
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

    fn invalidate(&mut self) {
        self.container.invalidate();
    }
}
