//! 单一 modal 仲裁 + 审批队列 + ask-user。
//!
//! # 不变量
//!
//! * 任意时刻最多只有一个 modal 处于活动状态（[`ModalArbiter::visible_modal_count`] ≤ 1）。
//! * 审批按 FIFO 排队；活动审批显示 `1/N`（N 包含活动项），解决一个后下一个变成 `1/N-1`。
//! * `Settings` modal 抢占当前 modal，但**不会丢**排队中的审批：被抢占的活动审批放回队首，
//!   关闭设置后恢复。
//! * 有 modal 时，键盘进入 modal；鼠标/滚动/尺寸事件被隔离（不转发给底层组件），
//!   备用屏状态下同样只渲染当前 modal，且 overlay 永不进入 history。
//!
//! # ask-user
//!
//! [`AskUserQuestion`] 支持：
//! * 单选（`Single`）；
//! * 多选（`Multi`，含 `All` 全选与 `Other` 自定义文本）；
//! * 纯文本（`Text`）；
//! * 是/否（`YesNo`，默认值可配置）。
//!
//! # 与参考实现的关系
//!
//! 队列/`index/total` 语义参考 ClaudeCode 的权限请求栈与 qwen-code 的确认对话框；
//! ask-user 的 `Other` 常驻语义参考 ClaudeCode `AskUserQuestionTool/prompt.ts`
//! （"Users will always be able to select Other"）。实现按 Kiana 的纯同步状态机重写。

use std::collections::VecDeque;

use crate::render_core::approval::{
    ApprovalCard, ApprovalDecision, ApprovalKey, ApprovalOutcome, ApprovalScope,
    ApprovalSuggestionKind,
};
use crate::render_core::frame::OverlayFrame;
use crate::render_core::i18n::Translator;
use kiana_tui::text::truncate_to_width;

/// modal 种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalKind {
    /// 权限审批。
    Approval,
    /// ask-user 问题。
    AskUser,
    /// 设置界面。
    Settings,
}

/// 键盘事件（modal 层）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalKey {
    /// 上。
    Up,
    /// 下。
    Down,
    /// 确认。
    Enter,
    /// 取消。
    Escape,
    /// 空格。
    Space,
    /// 退格。
    Backspace,
    /// 字符。
    Char(char),
}

/// 输入事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// 键盘。
    Key(ModalKey),
    /// 鼠标（本模块只做隔离，不解析坐标）。
    Mouse,
    /// 滚轮。
    Scroll(i32),
    /// 尺寸变化。
    Resize,
}

/// 输入路由结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Routed {
    /// 没有 modal，事件属于底层。
    Underlying,
    /// modal 消费了事件（可能产生了解析结果）。
    Modal(Option<ModalResolution>),
    /// modal 存在但不处理该事件；底层也不能收到。
    Ignored,
}

/// modal 产生的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModalResolution {
    /// 审批决定。
    Approval(ApprovalDecision),
    /// ask-user 的全部答案。
    AskUser(Vec<AskUserAnswer>),
    /// 设置关闭。
    SettingsClosed,
}

/// ask-user 选项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskUserOption {
    /// 标签。
    pub label: String,
    /// 说明。
    pub description: Option<String>,
}

impl AskUserOption {
    /// 只有标签的选项。
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            description: None,
        }
    }
}

/// ask-user 问题。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AskUserQuestion {
    /// 单选。
    Single {
        /// 问题文本。
        prompt: String,
        /// 选项。
        options: Vec<AskUserOption>,
    },
    /// 多选。
    Multi {
        /// 问题文本。
        prompt: String,
        /// 选项。
        options: Vec<AskUserOption>,
        /// 是否提供 All（全部选中）。
        allow_all: bool,
        /// 是否提供 Other（自定义文本）。
        allow_other: bool,
    },
    /// 文本输入。
    Text {
        /// 问题文本。
        prompt: String,
        /// 占位提示。
        placeholder: Option<String>,
    },
    /// 是/否。
    YesNo {
        /// 问题文本。
        prompt: String,
        /// 默认答案。
        default_yes: bool,
    },
}

impl AskUserQuestion {
    /// 问题文本。
    pub fn prompt(&self) -> &str {
        match self {
            Self::Single { prompt, .. }
            | Self::Multi { prompt, .. }
            | Self::Text { prompt, .. }
            | Self::YesNo { prompt, .. } => prompt,
        }
    }
}

/// ask-user 答案。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AskUserAnswer {
    /// 单选（`other` 非空时表示用户选择了 Other）。
    Single {
        /// 选项下标；Other 时为 `None`。
        index: Option<usize>,
        /// 标签。
        label: String,
        /// Other 文本。
        other: Option<String>,
    },
    /// 多选。
    Multi {
        /// 选中的选项下标。
        indices: Vec<usize>,
        /// 选中标签。
        labels: Vec<String>,
        /// Other 文本。
        other: Option<String>,
    },
    /// 文本。
    Text(String),
    /// 是/否。
    YesNo(bool),
}

#[derive(Clone, Debug)]
struct QuestionState {
    cursor: usize,
    selected: Vec<bool>,
    other_active: bool,
    other_text: String,
    text: String,
    yes: bool,
}

/// 多问题 ask-user 对话框。
#[derive(Clone, Debug)]
pub struct AskUserDialog {
    questions: Vec<AskUserQuestion>,
    states: Vec<QuestionState>,
    answers: Vec<Option<AskUserAnswer>>,
    current: usize,
    cancelled: bool,
}

impl AskUserDialog {
    /// 从问题列表构造。
    pub fn new(questions: Vec<AskUserQuestion>) -> Self {
        let states = questions
            .iter()
            .map(|question| {
                let option_count = match question {
                    AskUserQuestion::Single { options, .. } => options.len(),
                    AskUserQuestion::Multi {
                        options,
                        allow_all,
                        allow_other,
                        ..
                    } => options.len() + usize::from(*allow_all) + usize::from(*allow_other),
                    AskUserQuestion::Text { .. } | AskUserQuestion::YesNo { .. } => 1,
                };
                let yes = matches!(
                    question,
                    AskUserQuestion::YesNo {
                        default_yes: true,
                        ..
                    }
                );
                QuestionState {
                    cursor: 0,
                    selected: vec![false; option_count],
                    other_active: false,
                    other_text: String::new(),
                    text: String::new(),
                    yes,
                }
            })
            .collect();
        let answers = vec![None; questions.len()];
        Self {
            questions,
            states,
            answers,
            current: 0,
            cancelled: false,
        }
    }

    /// 当前问题下标。
    pub fn current(&self) -> usize {
        self.current
    }

    /// 问题总数。
    pub fn total(&self) -> usize {
        self.questions.len()
    }

    /// 是否完成。
    pub fn is_finished(&self) -> bool {
        self.answers.iter().all(Option::is_some)
    }

    /// 是否被取消。
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// 已收集的答案（未完成的位置为 `None`）。
    pub fn answers(&self) -> &[Option<AskUserAnswer>] {
        &self.answers
    }

    /// 处理按键；完成最后一个问题后返回全部答案。
    pub fn handle_key(&mut self, key: ModalKey) -> Option<Vec<AskUserAnswer>> {
        if self.is_finished() {
            return Some(self.collect());
        }
        if key == ModalKey::Escape {
            self.cancelled = true;
            return None;
        }
        let question = self.questions[self.current].clone();
        let state = &mut self.states[self.current];
        match &question {
            AskUserQuestion::Single { options, .. } => match key {
                ModalKey::Up => state.cursor = state.cursor.saturating_sub(1),
                ModalKey::Down => {
                    state.cursor = (state.cursor + 1).min(options.len().saturating_sub(1));
                }
                ModalKey::Enter => {
                    if let Some(option) = options.get(state.cursor) {
                        self.answers[self.current] = Some(AskUserAnswer::Single {
                            index: Some(state.cursor),
                            label: option.label.clone(),
                            other: None,
                        });
                        self.advance();
                    }
                }
                _ => {}
            },
            AskUserQuestion::Multi {
                options,
                allow_all,
                allow_other,
                ..
            } => {
                let all_index = options.len();
                let other_index = options.len() + usize::from(*allow_all);
                let total_rows =
                    options.len() + usize::from(*allow_all) + usize::from(*allow_other);
                if state.other_active {
                    match key {
                        ModalKey::Char(character) => state.other_text.push(character),
                        ModalKey::Space => state.other_text.push(' '),
                        ModalKey::Backspace => {
                            state.other_text.pop();
                        }
                        ModalKey::Enter => {
                            let indices: Vec<usize> = state
                                .selected
                                .iter()
                                .enumerate()
                                .filter_map(|(index, selected)| {
                                    (*selected && index < options.len()).then_some(index)
                                })
                                .collect();
                            let labels = indices
                                .iter()
                                .filter_map(|index| {
                                    options.get(*index).map(|option| option.label.clone())
                                })
                                .collect();
                            let other = (!state.other_text.trim().is_empty())
                                .then(|| state.other_text.trim().to_string());
                            self.answers[self.current] = Some(AskUserAnswer::Multi {
                                indices,
                                labels,
                                other,
                            });
                            self.advance();
                        }
                        _ => {}
                    }
                } else {
                    match key {
                        ModalKey::Up => state.cursor = state.cursor.saturating_sub(1),
                        ModalKey::Down => {
                            if total_rows > 0 {
                                state.cursor = (state.cursor + 1).min(total_rows - 1);
                            }
                        }
                        ModalKey::Space => {
                            if state.cursor < options.len() {
                                state.selected[state.cursor] = !state.selected[state.cursor];
                            } else if *allow_all && state.cursor == all_index {
                                let all_selected = state.selected.iter().all(|value| *value);
                                state.selected.fill(!all_selected);
                            } else if *allow_other && state.cursor == other_index {
                                state.other_active = true;
                            }
                        }
                        ModalKey::Char('a') | ModalKey::Char('A') if *allow_all => {
                            state.selected.fill(true);
                        }
                        ModalKey::Char('o') | ModalKey::Char('O') if *allow_other => {
                            state.other_active = true;
                        }
                        ModalKey::Enter => {
                            let indices: Vec<usize> = state
                                .selected
                                .iter()
                                .enumerate()
                                .filter_map(|(index, selected)| {
                                    (*selected && index < options.len()).then_some(index)
                                })
                                .collect();
                            let labels = indices
                                .iter()
                                .filter_map(|index| {
                                    options.get(*index).map(|option| option.label.clone())
                                })
                                .collect();
                            let other = (!state.other_text.trim().is_empty())
                                .then(|| state.other_text.trim().to_string());
                            self.answers[self.current] = Some(AskUserAnswer::Multi {
                                indices,
                                labels,
                                other,
                            });
                            self.advance();
                        }
                        _ => {}
                    }
                }
            }
            AskUserQuestion::Text { .. } => match key {
                ModalKey::Char(character) => state.text.push(character),
                ModalKey::Space => state.text.push(' '),
                ModalKey::Backspace => {
                    state.text.pop();
                }
                ModalKey::Enter => {
                    self.answers[self.current] = Some(AskUserAnswer::Text(state.text.clone()));
                    self.advance();
                }
                _ => {}
            },
            AskUserQuestion::YesNo { .. } => match key {
                ModalKey::Char('y') | ModalKey::Char('Y') | ModalKey::Up => state.yes = true,
                ModalKey::Char('n') | ModalKey::Char('N') | ModalKey::Down => state.yes = false,
                ModalKey::Space => state.yes = !state.yes,
                ModalKey::Enter => {
                    self.answers[self.current] = Some(AskUserAnswer::YesNo(state.yes));
                    self.advance();
                }
                _ => {}
            },
        }
        if self.is_finished() {
            Some(self.collect())
        } else {
            None
        }
    }

    fn advance(&mut self) {
        if self.current + 1 < self.questions.len() {
            self.current += 1;
        }
    }

    fn collect(&self) -> Vec<AskUserAnswer> {
        self.answers.iter().flatten().cloned().collect()
    }

    /// 渲染（无翻译器，英文原文）。
    pub fn render_lines(&self, width: usize) -> Vec<String> {
        self.render_lines_with(width, None)
    }

    /// 渲染；`translator` 为 `None` 时输出与 [`Self::render_lines`] 逐字一致。
    ///
    /// 设置翻译器后：问题位置用 `modal.question_position`，多选提示用
    /// `modal.multi_select_hint`，提交提示用 `modal.submit_hint`。选项标签与问题文本
    /// 来自扩展请求，保持原文。
    fn render_lines_with(&self, width: usize, translator: Option<&Translator>) -> Vec<String> {
        let Some(question) = self.questions.get(self.current) else {
            return Vec::new();
        };
        let state = &self.states[self.current];
        let header = match translator {
            Some(translator) => translator.t(
                "modal.question_position",
                &[
                    ("index", &(self.current + 1).to_string()),
                    ("total", &self.total().to_string()),
                ],
            ),
            None => format!("Question {}/{}", self.current + 1, self.total()),
        };
        let mut lines = vec![header, question.prompt().to_string()];
        let submit_hint = |translator: Option<&Translator>| match translator {
            Some(translator) => translator.t("modal.submit_hint", &[]),
            None => "Enter submit".to_string(),
        };
        match question {
            AskUserQuestion::Single { options, .. } => {
                for (index, option) in options.iter().enumerate() {
                    let marker = if index == state.cursor { ">" } else { " " };
                    lines.push(format!("{marker} {}. {}", index + 1, option.label));
                }
            }
            AskUserQuestion::Multi {
                options,
                allow_all,
                allow_other,
                ..
            } => {
                for (index, option) in options.iter().enumerate() {
                    let marker = if index == state.cursor { ">" } else { " " };
                    let checked = if state.selected[index] { "x" } else { " " };
                    lines.push(format!("{marker} [{checked}] {}", option.label));
                }
                if *allow_all {
                    let index = options.len();
                    let marker = if index == state.cursor { ">" } else { " " };
                    lines.push(format!("{marker} All"));
                }
                if *allow_other {
                    let index = options.len() + usize::from(*allow_all);
                    let marker = if index == state.cursor { ">" } else { " " };
                    lines.push(format!("{marker} Other: {}", state.other_text));
                }
                lines.push(match translator {
                    Some(translator) => translator.t("modal.multi_select_hint", &[]),
                    None => "Space toggle, a select all, o other, Enter submit".to_string(),
                });
            }
            AskUserQuestion::Text { placeholder, .. } => {
                let placeholder = placeholder.as_deref().unwrap_or("");
                lines.push(format!("> {}{}", state.text, placeholder));
                lines.push(submit_hint(translator));
            }
            AskUserQuestion::YesNo { .. } => {
                lines.push(format!(
                    "[{}] Yes  [{}] No  (y/n, {})",
                    if state.yes { "x" } else { " " },
                    if state.yes { " " } else { "x" },
                    submit_hint(translator)
                ));
            }
        }
        lines
            .into_iter()
            .map(|line| truncate_to_width(&line, width, "...", false))
            .collect()
    }
}

/// 设置 modal（最小占位；真正内容由接线层提供）。
#[derive(Clone, Debug, Default)]
pub struct SettingsModal {
    /// 展示行。
    pub lines: Vec<String>,
}

/// 活动 modal 的内容。
///
/// `ApprovalCard` 明显大于其它变体，装箱以避免枚举整体按最大变体取尺寸。
#[derive(Clone, Debug)]
enum ActiveContent {
    Approval(Box<ApprovalCard>),
    AskUser(AskUserDialog),
    Settings(SettingsModal),
}

#[derive(Clone, Debug)]
struct ActiveModal {
    id: u64,
    content: ActiveContent,
}

impl ActiveModal {
    fn kind(&self) -> ModalKind {
        match self.content {
            ActiveContent::Approval(_) => ModalKind::Approval,
            ActiveContent::AskUser(_) => ModalKind::AskUser,
            ActiveContent::Settings(_) => ModalKind::Settings,
        }
    }
}

/// 单一 modal 仲裁器。
#[derive(Debug, Default)]
pub struct ModalArbiter {
    next_id: u64,
    active: Option<ActiveModal>,
    approvals: VecDeque<ApprovalCard>,
    ask_users: VecDeque<AskUserDialog>,
    settings: Option<SettingsModal>,
    alt_screen: bool,
    /// 固定文案翻译器；`None` 时所有输出保持英文原文。
    translator: Option<Translator>,
}

impl ModalArbiter {
    /// 新建空仲裁器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置/替换翻译器（`modal.*` key）。未设置时输出与历史英文版本逐字一致。
    pub fn set_translator(&mut self, translator: Translator) {
        self.translator = Some(translator);
    }

    /// 切换 UI 语言时重译所有审批卡（活动卡 + 排队卡）。
    ///
    /// 审批卡的建议标签在构造时本地化，只替换 `modal.*` 翻译器不会更新已入队的卡片。
    pub fn set_approval_translator(&mut self, translator: &Translator) {
        if let Some(active) = self.active.as_mut()
            && let ActiveContent::Approval(card) = &mut active.content
        {
            card.relocalize(translator);
        }
        for card in &mut self.approvals {
            card.relocalize(translator);
        }
    }

    /// 是否存在活动 modal。
    pub fn is_modal_active(&self) -> bool {
        self.active.is_some()
    }

    /// 活动 modal 数量（恒 ≤ 1）。
    pub fn visible_modal_count(&self) -> usize {
        usize::from(self.active.is_some())
    }

    /// 活动 modal 种类。
    pub fn active_kind(&self) -> Option<ModalKind> {
        self.active.as_ref().map(ActiveModal::kind)
    }

    /// 活动 modal 的稳定 id（每次提升/抢占递增）。
    pub fn active_id(&self) -> Option<u64> {
        self.active.as_ref().map(|active| active.id)
    }

    /// 设置界面是否打开。
    pub fn settings_open(&self) -> bool {
        self.settings.is_some()
    }

    /// 审批队列长度（包含活动审批）。
    pub fn approval_count(&self) -> usize {
        self.approvals.len() + usize::from(self.active_kind() == Some(ModalKind::Approval))
    }

    /// 活动审批的 `(index, total)`；不是审批时返回 `None`。
    ///
    /// `index` 从 1 开始，`total` 包含当前活动项。
    pub fn approval_position(&self) -> Option<(usize, usize)> {
        (self.active_kind() == Some(ModalKind::Approval)).then(|| (1, self.approvals.len() + 1))
    }

    /// 入队一个审批；没有活动 modal 时立即成为活动项。
    pub fn enqueue_approval(&mut self, card: ApprovalCard) {
        self.approvals.push_back(card);
        self.promote_if_idle();
    }

    /// 入队一个 ask-user 对话框。
    pub fn enqueue_ask_user(&mut self, dialog: AskUserDialog) {
        self.ask_users.push_back(dialog);
        self.promote_if_idle();
    }

    /// 打开设置 modal（抢占当前 modal，但不丢队列）。
    pub fn open_settings(&mut self, settings: SettingsModal) {
        if self.active_kind() == Some(ModalKind::Settings) {
            self.settings = Some(settings.clone());
            if let Some(active) = self.active.as_mut() {
                active.content = ActiveContent::Settings(settings);
            }
            return;
        }
        self.settings = Some(settings.clone());
        if let Some(active) = self.active.take() {
            match active.content {
                ActiveContent::Approval(card) => self.approvals.push_front(*card),
                ActiveContent::AskUser(dialog) => self.ask_users.push_front(dialog),
                ActiveContent::Settings(_) => {}
            }
        }
        self.next_id += 1;
        self.active = Some(ActiveModal {
            id: self.next_id,
            content: ActiveContent::Settings(settings),
        });
    }

    /// 关闭设置并恢复被抢占的 modal。
    pub fn close_settings(&mut self) -> Option<ModalResolution> {
        if self.active_kind() != Some(ModalKind::Settings) {
            return None;
        }
        self.settings = None;
        self.active = None;
        self.promote_if_idle();
        Some(ModalResolution::SettingsClosed)
    }

    /// 解决当前活动 modal；审批返回用户决定，ask-user 返回答案列表。
    pub fn resolve_active(&mut self, decision: ApprovalDecision) -> Option<ModalResolution> {
        let active = self.active.take()?;
        let ActiveModal { id, content } = active;
        let resolution = match content {
            ActiveContent::Approval(_) => ModalResolution::Approval(decision),
            other => {
                // 非审批 modal 不能用审批决定关闭；恢复原状态。
                self.active = Some(ActiveModal { id, content: other });
                return None;
            }
        };
        self.promote_if_idle();
        Some(resolution)
    }

    /// 当前卡片的可变引用（测试/接线用）。
    pub fn active_approval_mut(&mut self) -> Option<&mut ApprovalCard> {
        match self.active.as_mut()?.content {
            ActiveContent::Approval(ref mut card) => Some(card.as_mut()),
            _ => None,
        }
    }

    /// 当前 ask-user 的可变引用。
    pub fn active_ask_user_mut(&mut self) -> Option<&mut AskUserDialog> {
        match self.active.as_mut()?.content {
            ActiveContent::AskUser(ref mut dialog) => Some(dialog),
            _ => None,
        }
    }

    /// 设置备用屏标记（只影响 overlay 合成与诊断）。
    pub fn set_alt_screen(&mut self, alt_screen: bool) {
        self.alt_screen = alt_screen;
    }

    /// 是否处于备用屏。
    pub fn alt_screen(&self) -> bool {
        self.alt_screen
    }

    /// 渲染当前活动 modal；没有活动项时返回空。
    pub fn render(&self, width: usize) -> Vec<String> {
        let Some(active) = self.active.as_ref() else {
            return Vec::new();
        };
        let mut lines = match &active.content {
            ActiveContent::Approval(card) => {
                let (index, total) = self.approval_position().unwrap_or((1, 1));
                let header = match self.translator.as_ref() {
                    Some(translator) => translator.t(
                        "modal.approval_position",
                        &[("index", &index.to_string()), ("total", &total.to_string())],
                    ),
                    None => format!("Approval {index}/{total}"),
                };
                let mut lines = vec![header];
                lines.extend(card.render_lines(width));
                lines
            }
            ActiveContent::AskUser(dialog) => {
                dialog.render_lines_with(width, self.translator.as_ref())
            }
            ActiveContent::Settings(settings) => {
                let title = match self.translator.as_ref() {
                    Some(translator) => translator.t("modal.settings_title", &[]),
                    None => "Settings".to_string(),
                };
                let mut lines = vec![title];
                lines.extend(settings.lines.iter().cloned());
                lines
            }
        };
        lines
            .drain(..)
            .map(|line| truncate_to_width(&line, width, "...", false))
            .collect()
    }

    /// 当前 modal 的 overlay（备用屏/普通屏都单独合成，绝不进入 history）。
    pub fn overlay_frame(&self, width: usize) -> Option<OverlayFrame> {
        let lines = self.render(width);
        if lines.is_empty() {
            return None;
        }
        Some(OverlayFrame {
            row: 0,
            column: 0,
            lines,
            full_screen: self.alt_screen,
        })
    }

    /// 路由输入：有 modal 时键盘进 modal，鼠标/滚动/尺寸均被隔离。
    pub fn route_input(&mut self, event: InputEvent) -> Routed {
        let Some(active) = self.active.as_mut() else {
            return Routed::Underlying;
        };
        let InputEvent::Key(key) = event else {
            // 鼠标 / 滚动 / resize：modal 存在时底层不能收到。
            return Routed::Ignored;
        };
        match &mut active.content {
            ActiveContent::Approval(card) => {
                let approval_key = match key {
                    ModalKey::Up => ApprovalKey::Up,
                    ModalKey::Down => ApprovalKey::Down,
                    ModalKey::Enter => ApprovalKey::Enter,
                    ModalKey::Escape => ApprovalKey::Escape,
                    ModalKey::Char(character) => ApprovalKey::Char(character),
                    ModalKey::Space | ModalKey::Backspace => return Routed::Ignored,
                };
                match card.handle_key(approval_key) {
                    ApprovalOutcome::Decision(decision) => {
                        self.active = None;
                        self.promote_if_idle();
                        Routed::Modal(Some(ModalResolution::Approval(decision)))
                    }
                    ApprovalOutcome::Moved | ApprovalOutcome::Ignored => Routed::Modal(None),
                }
            }
            ActiveContent::AskUser(dialog) => {
                let answers = dialog.handle_key(key);
                if let Some(answers) = answers {
                    self.active = None;
                    self.promote_if_idle();
                    return Routed::Modal(Some(ModalResolution::AskUser(answers)));
                }
                // Escape 只把 dialog 标记为 cancelled 并返回 None；这里必须结束这个
                // modal，否则它永远占据活动槽、吞掉后续所有按键，排队的审批也永远
                // 不会浮现。
                if dialog.is_cancelled() {
                    self.active = None;
                    self.promote_if_idle();
                }
                Routed::Modal(None)
            }
            ActiveContent::Settings(_) => {
                if key == ModalKey::Escape
                    && let Some(resolution) = self.close_settings()
                {
                    return Routed::Modal(Some(resolution));
                }
                Routed::Modal(None)
            }
        }
    }

    fn promote_if_idle(&mut self) {
        if self.active.is_some() {
            return;
        }
        if let Some(card) = self.approvals.pop_front() {
            self.next_id += 1;
            self.active = Some(ActiveModal {
                id: self.next_id,
                content: ActiveContent::Approval(Box::new(card)),
            });
            return;
        }
        if let Some(dialog) = self.ask_users.pop_front() {
            self.next_id += 1;
            self.active = Some(ActiveModal {
                id: self.next_id,
                content: ActiveContent::AskUser(dialog),
            });
        }
    }
}

/// 便捷：把 `ApprovalScope` 转成建议类型。
pub fn allow_always(scope: ApprovalScope) -> ApprovalSuggestionKind {
    ApprovalSuggestionKind::AllowAlways(scope)
}
