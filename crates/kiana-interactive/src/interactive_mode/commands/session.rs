//! `interactive-mode.ts` 的 `/session` 与选择器（user-message / tree / session）以及
//! `/clone`（§2.50.15、§2.50.15-16 的 5496–5783 行）。
//!
//! # 这个文件负责什么
//!
//! 会话域的四类交互命令：
//!
//! - `/session`：把会话统计（消息数、token、成本、缓存浪费、cache warming 状态）渲染成
//!   一段静态文本追加进聊天区（`handle_session_command`）。
//! - 会话树导航：`/tree` 与双击 Escape（`Tree` 动作）共用的 `show_tree_selector` /
//!   `navigate_tree_from_selector`，含「是否生成分支摘要」的三选一询问与自定义摘要
//!   指令输入。
//! - 消息级 fork：`show_user_message_selector`（`/fork` 与双击 Escape 的 `Fork` 动作）
//!   选择一条历史用户消息，`fork_to_user_message` 在其之前开新会话。
//! - 会话级克隆/切换：`handle_clone_command`（`/clone`，在当前叶子处 fork）与
//!   `show_session_selector`（`/resume`，列出磁盘上的会话文件供切换）。
//!
//! # 执行流程（以树导航为例）
//!
//! `navigate_tree_from_selector` 是本文件最长的流程：确认目标不是当前叶子 → 询问摘要
//! （`show_extension_selector` / `show_extension_editor`，取消则回到树选择器）→ 流式中
//! 先恢复队列消息并中止 → 压缩中直接拒绝 → 按需挂分支摘要指示器（Escape 挂
//! `abort_branch_summary`）→ `session.navigate_tree` → 成功后清空聊天区重放初始消息、
//! 恢复 editor_text、异步 flush 压缩队列 → 无论成败恢复 Escape 处理器与指示器。
//!
//! # 注意事项
//!
//! - 所有回调（选择/取消/复制/重命名等）只捕获 `Weak`；模式释放后升级失败即静默跳过。
//!   需要写 UI 的动作经 `tokio::task::spawn_local` 延后（见 `commands/mod.rs` 的模块文档）。
//! - 选择器通过 `show_selector` 的 `done` 回调关闭；`done.finish()` 的调用时机（选择前/
//!   取消时/树导航里先关再询问）逐处对照 TS，改动会破坏选择器能否正确关闭。
//! - `handle_session_command` 的渲染闭包由 `ThemedText` 惰性调用：统计数据在命令执行时
//!   就已快照进闭包，但文本要到首次渲染才生成，之后被缓存（主题失效时 `invalidate`
//!   会重建，`now_epoch_millis()` 也随之取新值）。因此它不会随会话变化自动刷新，再次
//!   查看需重跑 `/session`。

use std::rc::Rc;

use crate::tui_renderer::component_address;
use kiana_config::models::runtime::ModelRuntime;
use kiana_json::js_trim;
use kiana_session::agent_session::NavigateTreeOptions;
use kiana_session::agent_session_runtime::ForkSessionOptions;
use kiana_session_core::cache_stats::{CacheWasteTotals, ModelPriceInfo, ModelPriceSource};
use kiana_session_core::session_manager::{SessionListProgress, SessionManager};
use kiana_session_core::usage_totals::{UsageCostBreakdownEntry, get_usage_cost_breakdown};
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::tui::Component;
use kiana_ui_kit::themed_text::ThemedText;

use crate::components::compaction_summary_message::js_to_locale_string;
use crate::components::footer::format_tokens;
use crate::components::session_selector::{
    RenameSessionFn, SessionLoaderFuture, SessionSelectorComponent, SessionSelectorOptions,
};
use crate::components::status_indicator::BranchSummaryStatusIndicator;
use crate::components::tree_selector::TreeSelectorComponent;
use crate::components::user_message_selector::{UserMessageItem, UserMessageSelectorComponent};

use super::super::InteractiveMode;
use super::{SelectorCreation, SelectorFocus, shared_component};

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

/// `ModelRuntime` 的价格查询适配（`computeCacheWaste` 需要）。
///
/// 会话层的 `compute_cache_waste` 只关心 cache-read 单价，而价格的真实来源是
/// `ModelRuntime` 的模型目录；这个薄适配把二者解耦（trait 对象而非泛型，避免
/// `cache_stats` 依赖 `kiana-config`）。模型不在目录里时返回 `None`；统计层此时把读取
/// 单价按 0 估算，miss 仍会计入成本（高估浪费）。
struct RuntimePriceSource<'a>(&'a ModelRuntime);

impl ModelPriceSource for RuntimePriceSource<'_> {
    fn get_model(&self, provider: &str, model_id: &str) -> Option<ModelPriceInfo> {
        self.0
            .get_model(provider, model_id)
            .map(|model| ModelPriceInfo {
                cache_read: model.cost.cache_read,
            })
    }
}

/// `formatCacheWarmingStatus` 需要当前时间；`/session` 是即时渲染，用 `Date.now()`。
///
/// 在 `render_info` 闭包里调用，因此取的是该闭包被求值（`ThemedText` 首次渲染或其主题
/// 失效后重建）时的时刻，而非 `/session` 命令刚执行时；其余统计数据则是命令执行时的快照。
fn now_epoch_millis() -> i64 {
    kiana_ai::model_catalog::now_epoch_millis()
}

impl InteractiveMode {
    /// `handleSessionCommand()`。
    ///
    /// `/session` 命令：计算一次会话统计并渲染为静态文本块，追加到聊天区尾部（前面加
    /// 一个 `Spacer` 与上文隔开）。统计由三部分拼成：
    ///
    /// - `get_session_stats`：消息计数、token 计数、总成本、会话文件/id；
    /// - `compute_cache_waste`（经 `RuntimePriceSource` 取价）：缓存 miss 的 token 与
    ///   由此累计的多付成本（`CacheWasteTotals` 的 `missed_cost`），只在成本行展开时
    ///   显示；
    /// - `get_usage_cost_breakdown`：按 `provider/id` 分组的成本明细，仅当多于一个分组、
    ///   或唯一分组不等于当前模型时才展开（避免单模型会话重复显示总量）。
    ///
    /// 另外显示 cache warming 的模式与状态（含经济学决策的 miss/refresh 成本，当可用时）。
    /// 输入输出都是纯展示：不写会话、不改设置；文本里的数字格式化用 JS 语义助手
    /// （`js_to_locale_string` / `js_to_fixed`），这是当前稳定输出契约，改动需同步测试。
    pub(crate) fn handle_session_command(&mut self) {
        let session = self.session();
        let stats = session.get_session_stats();
        let session_name = self.session_manager().get_session_name();
        let entries = self.session_manager().get_entries();
        let model_runtime = session.model_runtime().clone();
        let cache_waste = kiana_session_core::cache_stats::compute_cache_waste(
            &entries,
            &RuntimePriceSource(&model_runtime),
        );
        let usage_breakdown = get_usage_cost_breakdown(&entries);

        let cache_warming_status = session.cache_warming_status();
        let cache_warming_mode = self.settings_manager().get_cache_warming_mode();
        let model = session.model();
        let selected_model_key = match &model {
            Some(model) => format!("{}/{}", model.provider, model.id),
            None => "undefined/undefined".to_string(),
        };

        let render_info = move || {
            // 渲染闭包：`ThemedText` 首次渲染时调用，结果缓存；主题失效重建时会再调用
            // 一次。统计值在命令执行时已捕获，`now_epoch_millis()` 在求值时取。
            let mut info = format!("{}\n\n", bold("Session Info"));
            if let Some(session_name) = session_name.as_deref().filter(|name| !name.is_empty()) {
                info.push_str(&format!(
                    "{} {session_name}\n",
                    fg(ThemeColor::Dim, "Name:")
                ));
            }
            info.push_str(&format!(
                "{} {}\n",
                fg(ThemeColor::Dim, "File:"),
                stats.session_file.as_deref().unwrap_or("In-memory")
            ));
            info.push_str(&format!(
                "{} {}\n\n",
                fg(ThemeColor::Dim, "ID:"),
                stats.session_id
            ));
            info.push_str(&format!("{}\n", bold("Messages")));
            info.push_str(&format!(
                "{} {}\n",
                fg(ThemeColor::Dim, "Total:"),
                stats.total_messages
            ));
            info.push_str(&format!(
                "{} {}\n",
                fg(ThemeColor::Dim, "User:"),
                stats.user_messages
            ));
            info.push_str(&format!(
                "{} {}\n",
                fg(ThemeColor::Dim, "Assistant:"),
                stats.assistant_messages
            ));
            info.push_str(&format!(
                "{} {} calls, {} results\n\n",
                fg(ThemeColor::Dim, "Tools:"),
                stats.tool_calls,
                stats.tool_results
            ));
            info.push_str(&format!("{}\n", bold("Tokens")));
            // prompt token = 输入 + cache-read + cache-write（与 TS 的展示口径一致）；
            // 只有存在缓存读写时才显示命中率与「Uncached」行。
            let input = stats.tokens.input;
            let cache_read = stats.tokens.cache_read;
            let cache_write = stats.tokens.cache_write;
            let prompt_tokens = input + cache_read + cache_write;
            info.push_str(&format!(
                "{} {}\n",
                fg(ThemeColor::Dim, "Input:"),
                js_to_locale_string(prompt_tokens)
            ));
            if prompt_tokens > 0.0 && (cache_read > 0.0 || cache_write > 0.0) {
                let hit_rate = fg(
                    ThemeColor::Dim,
                    &format!(
                        "({}%)",
                        kiana_ai::model_catalog::js_to_fixed(cache_read / prompt_tokens * 100.0, 1)
                    ),
                );
                info.push_str(&format!(
                    "  {} {} {hit_rate}\n",
                    fg(ThemeColor::Dim, "Cached:"),
                    js_to_locale_string(cache_read)
                ));
                let written = if cache_write > 0.0 {
                    format!(
                        " {}",
                        fg(
                            ThemeColor::Dim,
                            &format!("({} written to cache)", js_to_locale_string(cache_write))
                        )
                    )
                } else {
                    String::new()
                };
                info.push_str(&format!(
                    "  {} {}{written}\n",
                    fg(ThemeColor::Dim, "Uncached:"),
                    js_to_locale_string(input + cache_write)
                ));
            }
            info.push_str(&format!(
                "{} {}\n",
                fg(ThemeColor::Dim, "Output:"),
                js_to_locale_string(stats.tokens.output)
            ));
            info.push_str(&format!(
                "{} {}\n",
                fg(ThemeColor::Dim, "Total:"),
                js_to_locale_string(stats.tokens.total)
            ));

            info.push_str(&format!("\n{}\n", bold("Cache Warming")));
            info.push_str(&format!(
                "{} {}\n",
                fg(ThemeColor::Dim, "Mode:"),
                cache_warming_mode_str(cache_warming_mode)
            ));
            info.push_str(&format!(
                "{} {}\n",
                fg(ThemeColor::Dim, "Status:"),
                match &cache_warming_status {
                    Some(status) => kiana_config::cache::warmer::format_cache_warming_status(
                        status,
                        now_epoch_millis(),
                    ),
                    None => "Inactive (cache warming unavailable)".to_string(),
                }
            ));
            if let Some(decision) = cache_warming_status
                .as_ref()
                .and_then(|status| status.decision.as_ref())
                && decision.economics_available
            {
                info.push_str(&format!(
                    "{} ${}\n",
                    fg(ThemeColor::Dim, "Cache miss penalty:"),
                    kiana_ai::model_catalog::js_to_fixed(decision.miss_cost, 3)
                ));
                info.push_str(&format!(
                    "{} ${}\n",
                    fg(ThemeColor::Dim, "Refresh cost:"),
                    kiana_ai::model_catalog::js_to_fixed(decision.warm_cost, 3)
                ));
            }

            if stats.cost > 0.0 || cache_waste.missed_tokens > 0.0 {
                info.push_str(&format!("\n{}\n", bold("Cost")));
                info.push_str(&format!(
                    "{} ${}",
                    fg(ThemeColor::Dim, "Total:"),
                    kiana_ai::model_catalog::js_to_fixed(stats.cost, 3)
                ));
                let single_entry_repeats_total = usage_breakdown
                    .first()
                    .map(|entry| entry.key == selected_model_key)
                    .unwrap_or(false);
                if usage_breakdown.len() > 1 || !single_entry_repeats_total {
                    for entry in &usage_breakdown {
                        info.push_str(&format!(
                            "\n  {} ${} {}",
                            fg(ThemeColor::Dim, &format!("{}:", entry.key)),
                            kiana_ai::model_catalog::js_to_fixed(entry.cost, 3),
                            fg(
                                ThemeColor::Dim,
                                &format!("({} tokens)", format_tokens(number_to_u64(entry.tokens)))
                            )
                        ));
                    }
                }
                if cache_waste.missed_tokens > 0.0 {
                    let miss_label = if cache_waste.miss_count == 1 {
                        "1 miss".to_string()
                    } else {
                        format!("{} misses", cache_waste.miss_count)
                    };
                    let detail = format!(
                        "{} tokens, {miss_label}",
                        js_to_locale_string(cache_waste.missed_tokens)
                    );
                    if cache_waste.missed_cost >= 0.0001 {
                        info.push_str(&format!(
                            "\n{} ${} {}",
                            fg(ThemeColor::Dim, "Cache Re-billed:"),
                            kiana_ai::model_catalog::js_to_fixed(cache_waste.missed_cost, 3),
                            fg(ThemeColor::Dim, &format!("({detail})"))
                        ));
                    } else {
                        info.push_str(&format!(
                            "\n{} {detail}",
                            fg(ThemeColor::Dim, "Cache Re-billed:")
                        ));
                    }
                }
            }
            info
        };

        let chat = Rc::clone(&self.inner.borrow().chat_container);
        {
            let mut chat = chat.borrow_mut();
            chat.add_child(Box::new(Spacer::new()));
            chat.add_child(Box::new(ThemedText::with_padding(
                Box::new(render_info),
                1,
                0,
            )));
        }
        self.request_render();
    }

    /// `showUserMessageSelector()`。
    ///
    /// `/fork` 与双击 Escape 的 `Fork` 动作：列出当前会话里非空的用户消息
    /// （`get_user_messages_for_forking`，条目序即会话序），选中后在**该消息之前**开新
    /// 会话（`fork_to_user_message`）。没有可 fork 的消息时只显示状态行，不打开选择器。
    ///
    /// 焦点细节：该组件自身不处理输入，焦点必须直达内部消息列表，因此用
    /// [`SelectorFocus::Child`]——通过地址比对找出列表在容器里的下标（TS 的
    /// `focus: selector.getMessageList()`）。
    pub(crate) fn show_user_message_selector(&mut self) {
        let user_messages = self.session().get_user_messages_for_forking();
        if user_messages.is_empty() {
            self.show_status("No messages to fork from");
            return;
        }
        let initial_selected_id = user_messages.last().map(|(entry_id, _)| entry_id.clone());
        let items: Vec<UserMessageItem> = user_messages
            .into_iter()
            .map(|(entry_id, text)| UserMessageItem {
                id: entry_id,
                text,
                timestamp: None,
            })
            .collect();
        self.show_selector(move |mode, done| {
            let weak = mode.downgrade();
            let on_select_done = done.clone();
            let on_select: crate::components::user_message_selector::StringCallback =
                Box::new(move |entry_id: &str| {
                    on_select_done.finish();
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    let mut mode = InteractiveMode { inner };
                    let entry_id = entry_id.to_string();
                    tokio::task::spawn_local(async move {
                        mode.fork_to_user_message(&entry_id).await;
                    });
                });
            let weak_cancel = mode.downgrade();
            let on_cancel: crate::components::user_message_selector::VoidCallback =
                Box::new(move || {
                    done.finish();
                    if let Some(inner) = weak_cancel.upgrade() {
                        InteractiveMode { inner }.request_render();
                    }
                });
            let mut selector = UserMessageSelectorComponent::new(
                items.clone(),
                on_select,
                on_cancel,
                initial_selected_id.as_deref(),
            );
            // `focus: selector.getMessageList()`：找出消息列表在容器里的下标。
            //
            // 取列表组件的裸地址，再遍历容器的直接子组件比对地址；命中即停（回调返回
            // `false`）。初始选中项默认是最后一条用户消息（`initial_selected_id`）。
            let list_address = {
                let list = selector.get_message_list();
                list as *const crate::components::user_message_selector::UserMessageList
                    as *const () as usize
            };
            let mut index = 0usize;
            let mut focus_index = 0usize;
            selector.for_each_container_child(&mut |child| {
                if component_address(child) == list_address {
                    focus_index = index;
                    false
                } else {
                    index += 1;
                    true
                }
            });
            SelectorCreation {
                component: shared_component(selector),
                focus: SelectorFocus::Child(focus_index),
                dispose: None,
            }
        });
    }

    /// `showUserMessageSelector` 的 fork 分支。
    ///
    /// `entry_id` 是选中的用户消息条目 id。`fork`（`position: Before`，默认）把新会话
    /// 的叶子设成该消息的父条目，并把消息文本放进结果的 `selected_text` 供这里填回
    /// 编辑器——用户可以改完再发。三个出口：取消（只重绘）、成功（编辑器填回原文 +
    /// 状态行）、失败（`show_error`，会话不动）。fork 的完整语义（新建会话文件、
    /// teardown 当前会话、触发 `session_before_fork` 事件）在 `kiana-session` 的
    /// `AgentSessionRuntime::fork`。
    async fn fork_to_user_message(&mut self, entry_id: &str) {
        let runtime = Rc::clone(&self.inner.borrow().runtime_host);
        let result = runtime.fork(entry_id, None).await;
        match result {
            Ok(result) if result.cancelled => self.request_render(),
            Ok(result) => {
                self.inner
                    .borrow()
                    .editor
                    .set_text(result.selected_text.as_deref().unwrap_or(""));
                self.show_status("Forked to new session");
            }
            Err(error) => self.show_error(&error.to_string()),
        }
    }

    /// `handleCloneCommand()`。
    ///
    /// `/clone`：在当前会话的叶子处开新会话（`position: At`），编辑器清空。与
    /// `/fork` 的区别：不挑历史消息、不回填文本，等于「当前状态原样另起一份」。没有
    /// 叶子（会话还是空的）时只显示 `Nothing to clone yet`。
    pub(crate) async fn handle_clone_command(&mut self) {
        let Some(leaf_id) = self.session_manager().get_leaf_id().map(str::to_string) else {
            self.show_status("Nothing to clone yet");
            return;
        };
        let runtime = Rc::clone(&self.inner.borrow().runtime_host);
        let result = runtime
            .fork(
                &leaf_id,
                Some(ForkSessionOptions {
                    position: Some(kiana_ext_api::events::ForkPosition::At),
                    with_session: None,
                }),
            )
            .await;
        match result {
            Ok(result) if result.cancelled => self.request_render(),
            Ok(_) => {
                self.inner.borrow().editor.set_text("");
                self.show_status("Cloned to new session");
            }
            Err(error) => self.show_error(&error.to_string()),
        }
    }

    /// `showTreeSelector(initialSelectedId?)`。
    ///
    /// `/tree` 与双击 Escape 的 `Tree` 动作：展示会话树（`get_tree` 的快照），选中某个
    /// 条目后经 `navigate_tree_from_selector` 切换到该分支。会话为空时只显示状态行。
    /// 构造组件前先在闭包外读好：树快照、真实叶子 id（高亮用）、终端行数（决定可视
    /// 高度）、settings 的过滤模式与 `branchSummarySkipPrompt`。
    ///
    /// 三个用户操作都会接回本模式：
    /// - 选中 → 延后执行树导航（含摘要询问，见 `navigate_tree_from_selector`）；
    /// - 编辑标签 → 直接向 `SessionManager` 追加一条 `label_change` 条目并重绘
    ///   （标签是会话条目上的持久化元数据，不走选择器状态）；
    /// - 复制（`on_copy`）→ 延后写剪贴板；条目文本为空（含空串）时报错——见下。
    pub(crate) fn show_tree_selector(&mut self, initial_selected_id: Option<String>) {
        let tree = self.session_manager().get_tree();
        let real_leaf_id = self.session_manager().get_leaf_id().map(str::to_string);
        let initial_filter_mode = self.settings_manager().get_tree_filter_mode();
        if tree.is_empty() {
            self.show_status("No entries in session");
            return;
        }
        let terminal_rows = {
            let terminal = self.inner.borrow().ui.terminal();
            terminal.borrow().rows()
        };
        let skip_prompt = self.settings_manager().get_branch_summary_skip_prompt();
        self.show_selector(move |mode, done| {
            let weak = mode.downgrade();
            let select_done = done.clone();
            let on_select: crate::components::tree_selector::StringCallback =
                Box::new(move |entry_id: &str| {
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    let mut mode = InteractiveMode { inner };
                    let entry_id = entry_id.to_string();
                    let done = select_done.clone();
                    tokio::task::spawn_local(async move {
                        mode.navigate_tree_from_selector(entry_id, done, skip_prompt)
                            .await;
                    });
                });
            let weak_cancel = mode.downgrade();
            let on_cancel: crate::components::tree_selector::VoidCallback = Box::new(move || {
                done.finish();
                if let Some(inner) = weak_cancel.upgrade() {
                    InteractiveMode { inner }.request_render();
                }
            });
            let weak_label = mode.downgrade();
            let on_label_change: crate::components::tree_selector::LabelEditCallback =
                Box::new(move |entry_id: &str, label: Option<&str>| {
                    let Some(inner) = weak_label.upgrade() else {
                        return;
                    };
                    let mut mode = InteractiveMode { inner };
                    let session = mode.session();
                    let entry_id = entry_id.to_string();
                    let label = label.map(str::to_string);
                    session.with_session_manager_mut(|manager| {
                        let _ = manager.append_label_change(&entry_id, label.as_deref());
                    });
                    mode.request_render();
                });
            let mut selector = TreeSelectorComponent::new(
                tree.clone(),
                real_leaf_id.clone(),
                terminal_rows,
                on_select,
                on_cancel,
                Some(on_label_change),
                initial_selected_id.clone(),
                Some(tree_filter_mode(initial_filter_mode)),
            );
            let weak_copy = mode.downgrade();
            selector.on_copy = Some(Box::new(move |text: Option<&str>| {
                let Some(inner) = weak_copy.upgrade() else {
                    return;
                };
                let mut mode = InteractiveMode { inner };
                let text = text.map(str::to_string);
                tokio::task::spawn_local(async move {
                    // TS: `if (!text)` —— 空字符串同样走错误分支（`Some("")` 也要报错）。
                    let Some(text) = text.filter(|text| !text.is_empty()) else {
                        mode.show_error("Selected entry has no text to copy");
                        return;
                    };
                    match kiana_core::clipboard::copy_to_clipboard(&text).await {
                        Ok(()) => mode.show_status("Copied selected message to clipboard"),
                        Err(error) => mode.show_error(&error.to_string()),
                    }
                });
            }));
            SelectorCreation {
                component: shared_component(selector),
                focus: SelectorFocus::Component,
                dispose: None,
            }
        });
    }

    /// `showTreeSelector` 的选中处理器（摘要询问 → 终止流式 → `navigateTree`）。
    ///
    /// # 执行顺序（每步都对应 TS 的调用次序，不要调换）
    ///
    /// 1. 目标就是当前叶子：`done.finish()` 后只显示 `Already at this point`。
    /// 2. `done.finish()` 先关选择器，再做后面可能等待用户的步骤——摘要询问期间
    ///    屏幕上不应残留树选择器。
    /// 3. 摘要询问（`skip_prompt` 为假时）：三选一（不总结/总结/自定义指令）。取消选项
    ///    回到树选择器（重新挂载，保持上下文）；选自定义指令后输入被取消则**重新询问**
    ///    （`loop` + `continue`），因为此时既没有摘要也没有确认，直接退出会丢上下文。
    /// 4. 流式中：把 steering/follow-up 队列并回编辑器并 `abort()`；压缩中：直接报错
    ///    返回（树导航与压缩互斥，`navigate_tree` 在会话层也会拒绝）。两项检查都在摘要
    ///    询问**之后**才做：对话框打开期间流式/压缩可能已结束（tests/
    ///    `interactive_mode_tree_navigation.rs` 的「compaction finishes while the dialog
    ///    is open」用例锁定此时仍可继续导航）。
    /// 5. 需要摘要时挂 `BranchSummaryStatusIndicator`，并把默认编辑器的 Escape 处理器
    ///    临时换成 `abort_branch_summary`（原文保存，最后无条件恢复，即使导航失败）。
    /// 6. `session.navigate_tree`；结果分支见下。
    ///
    /// # 结果分支
    ///
    /// - `aborted`：用户在摘要生成中按了 Escape → 显示取消状态并**重新打开树选择器**。
    /// - `cancelled`：导航被会话层取消 → 只显示状态。
    /// - 成功：清空聊天区、重放初始消息；`editor_text`（目标为用户消息时的文本）只在
    ///   编辑器当前为空（JS `trim` 语义）时填入，不覆盖用户正在输入的内容；随后延后
    ///   flush 压缩队列。
    /// - `Err`：`show_error`，会话保持在原位置。
    ///
    /// 注意第 5 步的指示器与 Escape 恢复放在 `match` 之后无条件执行，任何一条出口
    /// 都不会泄漏临时处理器。
    async fn navigate_tree_from_selector(
        &mut self,
        entry_id: String,
        done: super::SelectorDone,
        skip_prompt: bool,
    ) {
        if self
            .session_manager()
            .get_leaf_id()
            .map(str::to_string)
            .as_deref()
            == Some(entry_id.as_str())
        {
            done.finish();
            self.show_status("Already at this point");
            return;
        }

        done.finish();

        let mut wants_summary = false;
        let mut custom_instructions: Option<String> = None;
        if !skip_prompt {
            loop {
                let summary_choice = self
                    .show_extension_selector(
                        "Summarize branch?",
                        &[
                            "No summary".to_string(),
                            "Summarize".to_string(),
                            "Summarize with custom prompt".to_string(),
                        ],
                        None,
                    )
                    .await;
                let Some(summary_choice) = summary_choice else {
                    self.show_tree_selector(Some(entry_id));
                    return;
                };
                wants_summary = summary_choice != "No summary";
                if summary_choice == "Summarize with custom prompt" {
                    custom_instructions = self
                        .show_extension_editor("Custom summarization instructions")
                        .await;
                    if custom_instructions.is_none() {
                        continue;
                    }
                }
                break;
            }
        }

        if self.session().is_streaming() {
            self.restore_queued_messages_to_editor(false, None);
            self.session().abort().await;
        }

        if self.session().is_compacting() {
            self.show_error(
                "Wait for the current compaction or tree navigation to finish before navigating the session tree.",
            );
            return;
        }

        let mut showing_summary_indicator = false;
        let original_on_escape = self.default_editor().borrow_mut().take_on_escape();
        if wants_summary {
            let session = self.session();
            self.default_editor()
                .borrow_mut()
                .set_on_escape(Some(Box::new(move || {
                    session.abort_branch_summary();
                })));
            let chat = Rc::clone(&self.inner.borrow().chat_container);
            chat.borrow_mut().add_child(Box::new(Spacer::new()));
            let indicator = BranchSummaryStatusIndicator::new(Some(self.request_render_callback()));
            self.show_status_indicator(Box::new(indicator));
            showing_summary_indicator = true;
            self.request_render();
        }

        let session = self.session();
        let result = session
            .navigate_tree(
                entry_id.clone(),
                NavigateTreeOptions {
                    summarize: Some(wants_summary),
                    custom_instructions: custom_instructions.clone(),
                    replace_instructions: None,
                    label: None,
                },
            )
            .await;

        match result {
            Ok(result) if result.aborted.unwrap_or(false) => {
                self.show_status("Branch summarization cancelled");
                self.show_tree_selector(Some(entry_id));
            }
            Ok(result) if result.cancelled => {
                self.show_status("Navigation cancelled");
            }
            Ok(result) => {
                let chat = Rc::clone(&self.inner.borrow().chat_container);
                chat.borrow_mut().clear();
                self.render_initial_messages();
                if let Some(editor_text) = result.editor_text.as_deref()
                    && js_trim(&self.inner.borrow().editor.get_text()).is_empty()
                {
                    self.inner.borrow().editor.set_text(editor_text);
                }
                self.show_status("Navigated to selected point");
                let mut mode = self.clone();
                tokio::task::spawn_local(async move {
                    mode.flush_compaction_queue(false).await;
                });
            }
            Err(error) => self.show_error(&error.to_string()),
        }

        if showing_summary_indicator {
            self.clear_status_indicator(Some(
                crate::components::status_indicator::StatusIndicatorKind::BranchSummary,
            ));
        }
        self.default_editor()
            .borrow_mut()
            .set_on_escape(original_on_escape);
    }

    /// `showSessionSelector()`。
    ///
    /// `/resume`：列出磁盘上的会话文件供切换。本函数只装配依赖，选择器组件负责真正的
    /// 列表加载与展示：
    ///
    /// - `request_render`：选择器在 `Arc` 回调线程上无法直接碰 `RefCell` 的 UI，改用
    ///   unbounded channel 把重绘请求转发回本地任务（`spawn_local` 的消费循环）。
    /// - `current_loader` / `all_loader`：两个目录列表器。「当前目录」即 cwd 下默认或
    ///   自定义的会话目录；「全部会话」在自定义目录模式下只列该目录
    ///   （`list_all_in`），默认目录模式下列所有 cwd（`list_all`）——与 TS 的
    ///   `listAllSessions` 分派一致。
    /// - `rename_session_for_selector`：为选中的会话文件追加一条 session-info 重命名
    ///   记录。名字经 JS `trim`，空名字直接忽略（不报错）；打开文件失败也静默忽略
    ///   （重命名是尽力而为的边路操作，不应打断选择器）。
    ///
    /// 选中 → 延后 `handle_resume_session`；取消 → 关闭选择器；exit → 延后
    /// `shutdown(false)` 正常退出进程。
    pub(crate) fn show_session_selector(&mut self) {
        let session = self.session();
        let cwd = self.session_manager().get_cwd().to_string();
        let session_dir = self.session_manager().get_session_dir().to_string();
        let uses_default_session_dir =
            session.with_session_manager_mut(|manager| manager.uses_default_session_dir());
        let current_session_file = self
            .session_manager()
            .get_session_file()
            .map(str::to_string);
        let keybindings = (*self.inner.borrow().keybindings).clone();
        let (render_sender, mut render_receiver) = tokio::sync::mpsc::unbounded_channel::<()>();
        {
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                while render_receiver.recv().await.is_some() {
                    if let Some(inner) = weak.upgrade() {
                        InteractiveMode { inner }.request_render();
                    }
                }
            });
        }
        let request_render: crate::components::session_selector::RequestRenderFn =
            std::sync::Arc::new(move || {
                let _ = render_sender.send(());
            });

        let current_loader: crate::components::session_selector::SessionsLoader = {
            // 「当前目录」列表器：cwd 固定，目录在构造时已捕获；每次调用重新装配闭包
            // 以便选择器可以反复触发加载。
            let cwd = cwd.clone();
            let session_dir = session_dir.clone();
            std::sync::Arc::new(
                move |on_progress: Option<SessionListProgress>,
                      signal: Option<kiana_ai::utils::abort::AbortScope>| {
                    let cwd = cwd.clone();
                    let session_dir = session_dir.clone();
                    let future: SessionLoaderFuture = Box::pin(async move {
                        SessionManager::list(&cwd, Some(&session_dir), on_progress, signal.as_ref())
                            .await
                    });
                    future
                },
            )
        };
        let all_loader: crate::components::session_selector::SessionsLoader = {
            // 「全部会话」列表器：默认目录模式下列所有 cwd 的会话，自定义目录模式下
            // 只列该目录（`uses_default_session_dir` 在构造时判定一次）。
            let session_dir = session_dir.clone();
            std::sync::Arc::new(
                move |on_progress: Option<SessionListProgress>,
                      signal: Option<kiana_ai::utils::abort::AbortScope>| {
                    let session_dir = session_dir.clone();
                    let future: SessionLoaderFuture = Box::pin(async move {
                        if uses_default_session_dir {
                            SessionManager::list_all(on_progress, signal.as_ref()).await
                        } else {
                            SessionManager::list_all_in(&session_dir, on_progress, signal.as_ref())
                                .await
                        }
                    });
                    future
                },
            )
        };

        let rename_session_for_selector: RenameSessionFn =
            Box::new(|session_file_path: &str, next_name: &str| {
                let next = js_trim(next_name);
                if next.is_empty() {
                    return;
                }
                if let Ok(mut manager) = SessionManager::open(session_file_path, None, None) {
                    let _ = manager.append_session_info(next);
                }
            });
        self.show_selector(move |mode, done| {
            let weak = mode.downgrade();
            let select_done = done.clone();
            let on_select: crate::components::session_selector::StringCallback =
                Box::new(move |session_path: &str| {
                    select_done.finish();
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    let mut mode = InteractiveMode { inner };
                    let session_path = session_path.to_string();
                    tokio::task::spawn_local(async move {
                        mode.handle_resume_session(session_path, None).await;
                    });
                });
            let weak_cancel = mode.downgrade();
            let on_cancel: crate::components::session_selector::VoidCallback =
                Box::new(move || {
                    done.finish();
                    if let Some(inner) = weak_cancel.upgrade() {
                        InteractiveMode { inner }.request_render();
                    }
                });
            let weak_exit = mode.downgrade();
            let on_exit: crate::components::session_selector::VoidCallback = Box::new(move || {
                if let Some(inner) = weak_exit.upgrade() {
                    let mut mode = InteractiveMode { inner };
                    tokio::task::spawn_local(async move {
                        mode.shutdown(false).await;
                    });
                }
            });
            let selector = SessionSelectorComponent::new(
                current_loader.clone(),
                all_loader.clone(),
                on_select,
                on_cancel,
                on_exit,
                request_render.clone(),
                SessionSelectorOptions {
                    rename_session: Some(rename_session_for_selector),
                    show_rename_hint: Some(true),
                    keybindings: Some(keybindings.clone()),
                },
                current_session_file.clone(),
            );
            SelectorCreation {
                component: shared_component(selector),
                focus: SelectorFocus::Component,
                dispose: None,
            }
        });
    }

    /// `handleSessionCommand` 的测试接缝。
    #[doc(hidden)]
    pub fn test_handle_session_command(&mut self) {
        self.handle_session_command();
    }

    /// `showTreeSelector` 的测试接缝。
    #[doc(hidden)]
    pub fn test_show_tree_selector(&mut self, initial_selected_id: Option<String>) {
        self.show_tree_selector(initial_selected_id);
    }

    /// `showUserMessageSelector` 的测试接缝。
    #[doc(hidden)]
    pub fn test_show_user_message_selector(&mut self) {
        self.show_user_message_selector();
    }

    /// `showSessionSelector` 的测试接缝。
    #[doc(hidden)]
    pub fn test_show_session_selector(&mut self) {
        self.show_session_selector();
    }

    /// `navigate_tree_from_selector` 的测试接缝（跳过选择器组件）。
    #[doc(hidden)]
    pub async fn test_navigate_tree_from_selector(&mut self, entry_id: String, skip_prompt: bool) {
        self.navigate_tree_from_selector(entry_id, super::SelectorDone::noop(), skip_prompt)
            .await;
    }

    /// `handleCloneCommand` 的测试接缝。
    #[doc(hidden)]
    pub async fn test_handle_clone_command(&mut self) {
        self.handle_clone_command().await;
    }

    /// 供测试断言：`computeCacheWaste` 的包装。
    #[doc(hidden)]
    pub fn test_compute_cache_waste(&self) -> CacheWasteTotals {
        let entries = self.session_manager().get_entries();
        let runtime = self.session().model_runtime().clone();
        kiana_session_core::cache_stats::compute_cache_waste(
            &entries,
            &RuntimePriceSource(&runtime),
        )
    }

    /// 供测试断言：`getUsageCostBreakdown` 的包装。
    #[doc(hidden)]
    pub fn test_usage_cost_breakdown(&self) -> Vec<UsageCostBreakdownEntry> {
        get_usage_cost_breakdown(&self.session_manager().get_entries())
    }
}

/// `cacheWarmingMode` 的字符串形式。
///
/// 输出直接进入 `/session` 的文本块，拼写是用户可见契约（settings 原文是小写单词）。
fn cache_warming_mode_str(mode: kiana_config::settings::CacheWarmingMode) -> &'static str {
    match mode {
        kiana_config::settings::CacheWarmingMode::Off => "off",
        kiana_config::settings::CacheWarmingMode::Streaming => "streaming",
        kiana_config::settings::CacheWarmingMode::Idle => "idle",
    }
}

/// `formatTokens(number)`：`format_tokens` 接受 `u64`。
///
/// TS 的 `formatTokens` 接受 `number`；Rust 侧把 `f64` 截断为 `u64`。非有限值与
/// 非正数都归零（成本明细里的 token 计数不该出现这些值，归零是保守显示）。
fn number_to_u64(value: f64) -> u64 {
    if value.is_finite() && value > 0.0 {
        value as u64
    } else {
        0
    }
}

/// settings 的 `TreeFilterMode` → 选择器的 `FilterMode`。
///
/// 两个枚举成员一一对应（类型上分开是为了让 settings 层不依赖选择器组件）；新增
/// settings 变体时必须同步在这里补映射，否则编译失败提醒。
fn tree_filter_mode(
    mode: kiana_config::settings::TreeFilterMode,
) -> crate::components::tree_selector::FilterMode {
    use kiana_config::settings::TreeFilterMode;
    match mode {
        TreeFilterMode::Default => crate::components::tree_selector::FilterMode::Default,
        TreeFilterMode::NoTools => crate::components::tree_selector::FilterMode::NoTools,
        TreeFilterMode::UserOnly => crate::components::tree_selector::FilterMode::UserOnly,
        TreeFilterMode::LabeledOnly => crate::components::tree_selector::FilterMode::LabeledOnly,
        TreeFilterMode::All => crate::components::tree_selector::FilterMode::All,
    }
}
