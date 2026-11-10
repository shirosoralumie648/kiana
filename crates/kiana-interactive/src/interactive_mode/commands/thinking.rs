//! `interactive-mode.ts` 的 `/thinking`（§2.50.10）。
//!
//! `handleThinkingCommand` / `selectThinkingLevel` / `showThinkingSelector`。
//!
//! # 为什么需要这个文件
//!
//! 思考等级（thinking level）有三个来源，本文件负责把它们接到交互层：
//! 会话当前的运行时等级（`AgentSession::thinking_level`）、当前模型支持的等级集合
//! （`get_available_thinking_levels`，由模型能力决定），以及 settings 里的“全局默认级别”。
//! `/thinking` 命令与 `/settings` 的“Default thinking level per model”子菜单最终都落到
//! 同一组副作用：写会话等级 + 刷新 footer/编辑器边框；状态行提示（`Thinking level: …` /
//! `Default thinking level: …`）只有 `/thinking` 路径有，per-model 子菜单路径不显示状态行
//! （其回调只写设置 + `set_thinking_level` + footer 失效 + `update_editor_border_color`，
//! 见 `settings.rs` 的 `on_model_thinking_level_change` / `on_model_thinking_level_remove`）。
//!
//! # 执行流程
//!
//! 1. `commands/mod.rs` 的提交分派匹配到 `/thinking` 前缀后清空编辑器，把参数（已按 JS
//!    `trim` 处理）或 `None` 交给 `handle_thinking_command`；
//! 2. 无参数 → `show_thinking_selector` 打开选择器（组件：`components/thinking_selector.rs`）；
//!    有参数 → 只在 `available_levels` 内做小写精确匹配，命中即
//!    `select_thinking_level(persist = false)`，未命中则把原样参数与可用级别列表拼进错误信息；
//! 3. 选择器里按确认键走 `on_select`（`persist = false`）；按 `app.thinking.save` 键走
//!    `on_select_as_default`（`persist = true`）。两条路径都先执行动作、再 `done.finish()`
//!    关闭选择器。
//!
//! # 注意事项
//!
//! * 匹配范围是**当前模型可用的等级**，不是全部 7 个等级；错误信息里的可用列表来自同一份
//!   会话查询（与 TS `availableLevels.join(", ")` 一致）。非 reasoning 模型的可用集合只有
//!   `off`，因此只能匹配 `off`。
//! * `select_thinking_level` 调用的 `AgentSession::set_thinking_level` 返回 `()`：会话实现
//!   内部用 `let _ =` 丢弃了 `set_thinking_level_impl` 的错误（写会话文件/追加记录失败），
//!   因此状态行总会照常显示，无法从这里判断写盘是否成功。
//! * 打开选择器时读取的是**快照**：`available_levels` 被 clone 进组件，会话期间切换模型
//!   不会刷新已打开的选择器。
//! * `get_available_thinking_levels` 在没有当前模型时返回全部等级（
//!   `THINKING_LEVEL_OPTIONS`），有模型时返回该模型支持的子集（见
//!   `kiana-session` 的 `agent_session.rs`）；因此“未登录/无模型”时命令能匹配全部 7 个值。
//! * 选择器用 `SelectorFocus::Component`（组件自身实现 `Focusable`），与 `/settings`
//!   指向内部列表的做法不同。

use kiana_agent_core::ThinkingLevel;
use kiana_config::defaults::DEFAULT_THINKING_LEVEL;
use kiana_json::js_trim;

use crate::components::thinking_selector::ThinkingSelectorComponent;

use super::super::InteractiveMode;
use super::{SelectorCreation, SelectorFocus, level_str, shared_component};

impl InteractiveMode {
    /// `handleThinkingCommand(searchTerm?)`。
    ///
    /// `search_term` 为 `None` 时打开选择器；否则按“小写精确匹配”在**当前模型可用**的等级
    /// 里查一项（`level_str` 输出即小写字面量），命中则立即应用且不持久化
    /// （`persist = false`）。
    ///
    /// 注意：`available_levels` 在 `None` 分支之前读取，但只有带参数的分支会用到；没有当前
    /// 模型时会话返回全部等级（见模块文档）。
    ///
    /// 未命中时调用 `show_error`，文案保留用户输入的**原样**（不 trim、不小写），并附上
    /// 用 `", "` 连接的可用级别列表；这是 TS 的逐字文案，测试会断言 `Unknown thinking level`
    /// 前缀。可用集合由会话给出：非 reasoning 模型只有 `off`
    /// （`kiana_ai::models::thinking::get_supported_thinking_levels`），所以输入其它等级会
    /// 落进这条错误分支。
    ///
    /// 注意：`None` 与 `Some("")` 在本函数里走不同分支——空串也算“有参数”，会进行匹配并
    /// 报错。经提交分派时不会出现空串：`dispatch_submit` 先对整段文本做 JS `trim`，
    /// `/thinking ` 会退化成 `/thinking` 并停在 `None` 分支。直接调用本函数（测试接缝）时
    /// 需要注意这一差别。
    pub(crate) fn handle_thinking_command(&mut self, search_term: Option<String>) {
        let available_levels = self.session().get_available_thinking_levels();
        let Some(search_term) = search_term else {
            self.show_thinking_selector();
            return;
        };

        let normalized = js_trim(&search_term).to_lowercase();
        // `level_str` 返回的正是小写字面量，因此这里只需把输入归一化成 `trim + lowercase`；
        // `available_levels` 的顺序就是错误信息与选择器列表的顺序。
        let level = available_levels
            .iter()
            .copied()
            .find(|candidate| level_str(*candidate) == normalized);
        let Some(level) = level else {
            let levels: Vec<String> = available_levels
                .iter()
                .map(|level| level_str(*level))
                .collect();
            self.show_error(&format!(
                "Unknown thinking level \"{search_term}\". Available levels: {}.",
                levels.join(", ")
            ));
            return;
        };

        self.select_thinking_level(level, false);
    }

    /// `selectThinkingLevel(level, persist)`。
    ///
    /// 唯一的“应用思考等级”入口，命令与选择器共用。`persist` 决定是否写入全局默认值
    /// （`ModelMutationOptions::persist`）。副作用按固定顺序：
    ///
    /// 1. `session.set_thinking_level`：会话先把请求的等级与可用集合比对，不在集合内时
    ///    经 `clamp_thinking_level` 收窄到当前模型支持的值；`persist` 为真时写进 settings
    ///    的是**请求**的等级，而不是收窄后的生效值（见 `agent_session.rs` 的
    ///    `set_thinking_level_impl`）；
    /// 2. footer 失效 + `update_editor_border_color`：边框颜色随等级变化；
    /// 3. 状态行：`persist` 为真显示 `Default thinking level: <level>`，否则
    ///    `Thinking level: <level>`；其中的 `<level>` 同样是请求值，因此在被收窄的场景下
    ///    可能与实际生效值不同。
    ///
    /// 失败模式：`set_thinking_level` 返回 `()`，会话内部 `let _ =` 丢弃了写会话文件/追加
    /// 记录的 `Err`（见 `kiana-session` 的 `agent_session.rs`），因此这里没有错误分支、
    /// 状态行无条件显示。等级字符串来自 `super::level_str`（用户可见拼写）。
    pub(crate) fn select_thinking_level(&mut self, level: ThinkingLevel, persist: bool) {
        self.session().set_thinking_level(
            level,
            kiana_session::agent_session::ModelMutationOptions {
                persist: Some(persist),
            },
        );
        self.inner.borrow().footer.borrow_mut().invalidate();
        self.update_editor_border_color();
        self.show_status(&if persist {
            format!("Default thinking level: {}", level_str(level))
        } else {
            format!("Thinking level: {}", level_str(level))
        });
    }

    /// `showThinkingSelector()`。
    ///
    /// 读取三个快照后交给 `show_selector`：会话当前等级（预选）、当前模型可用等级集合、
    /// settings 的默认等级（缺失时用 `DEFAULT_THINKING_LEVEL`）。后两者语义不同：
    /// 可用集合决定列表里有哪些项，默认等级只用于给组件标注 “· default”。
    ///
    /// 组件构造的 6 个参数与 TS 一一对应（旧版规格漏掉了最后两个参数：`on_select_as_default`
    /// 与默认等级，见 spec/ca-interactive-mode.md 行 3728）：
    /// 当前等级、可用等级、`on_select`（不持久化）、`on_cancel`、
    /// `on_select_as_default`（持久化）、默认等级。
    ///
    /// 两个选择回调都先经 `Weak` 升级回 [`InteractiveMode`] 再调 `select_thinking_level`，
    /// 然后**无条件** `done.finish()`：即使模式已被释放，选择器也会关闭（`done` 内部同样
    /// 升级失败即返回），不会留下一个无法关闭的 UI。取消回调只请求一次重绘。
    ///
    /// 焦点用 `SelectorFocus::Component`，因为 `ThinkingSelectorComponent` 自身实现
    /// `Focusable`（TS 的 `focus: selector`），与 `/settings` 指向内部列表不同。
    ///
    /// 边界：回调触发时选择器正在自己的 `handle_input` 栈里，`done.finish()` 会延后恢复
    /// 编辑器槽位（见 `commands::show_selector` 的文档）；因此这里不在回调里同步重建 UI。
    pub(crate) fn show_thinking_selector(&mut self) {
        let current_level = self.session().thinking_level();
        let available_levels = self.session().get_available_thinking_levels();
        let default_level = self
            .settings_manager()
            .get_default_thinking_level()
            .unwrap_or(DEFAULT_THINKING_LEVEL);
        self.show_selector(move |mode, done| {
            let weak = mode.downgrade();
            let select_level = {
                let weak = weak.clone();
                let done = done.clone();
                move |level: ThinkingLevel, persist: bool| {
                    if let Some(inner) = weak.upgrade() {
                        let mut mode = InteractiveMode { inner };
                        mode.select_thinking_level(level, persist);
                    }
                    done.finish();
                }
            };
            let on_select = {
                let select_level = select_level.clone();
                move |level: ThinkingLevel| select_level(level, false)
            };
            let on_select_as_default: Box<dyn FnMut(ThinkingLevel)> = {
                let select_level = select_level.clone();
                Box::new(move |level: ThinkingLevel| select_level(level, true))
            };
            let on_cancel = {
                let weak = weak.clone();
                let done = done.clone();
                move || {
                    done.finish();
                    if let Some(inner) = weak.upgrade() {
                        InteractiveMode { inner }.request_render();
                    }
                }
            };
            let selector = ThinkingSelectorComponent::new(
                current_level,
                available_levels.clone(),
                on_select,
                on_cancel,
                Some(on_select_as_default),
                Some(default_level),
            );
            SelectorCreation {
                component: shared_component(selector),
                focus: SelectorFocus::Component,
                dispose: None,
            }
        });
    }

    /// `showThinkingSelector()` 的测试接缝（集成测试与生产代码分属不同 crate，`pub(crate)`
    /// 不可见，故声明为 `pub` + `#[doc(hidden)]`，只做转发）。
    #[doc(hidden)]
    pub fn test_show_thinking_selector(&mut self) {
        self.show_thinking_selector();
    }

    /// 供测试断言：`selectThinkingLevel`（转发，见上方语义）。
    #[doc(hidden)]
    pub fn test_select_thinking_level(&mut self, level: ThinkingLevel, persist: bool) {
        self.select_thinking_level(level, persist);
    }

    /// 供测试断言：`handleThinkingCommand`。
    #[doc(hidden)]
    pub fn test_handle_thinking_command(&mut self, search_term: Option<String>) {
        self.handle_thinking_command(search_term);
    }
}
