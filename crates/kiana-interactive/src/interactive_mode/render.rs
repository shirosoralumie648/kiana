//! `interactive-mode.ts` 的聊天区渲染 / 状态行 / 用户输入（§2.50.6）。
//!
//! 拆分协议：`interactive_mode.rs` 只声明了 CONTRACT 里的入口；这里实现它们，并补充
//! 事件处理 / 命令分派需要的渲染辅助（消息分派、条目重建、成本与缓存提示、队列展示）。
//!
//! 借用纪律见 `interactive_mode.rs` 文件头：所有方法只在同步短作用域里借用 `inner`，
//! 组件句柄先克隆出来再操作。
//!
//! # 这个文件负责什么
//! 把会话条目与运行期事件翻译成聊天容器里的组件树：历史重建
//! （`render_initial_messages` / `render_session_entries` / `render_session_items`）、
//! 单条消息分派（`add_message_to_chat`）、状态与提示行（`show_status` / `show_error` /
//! 缓存与版本提示），以及 `get_user_input` 的 oneshot 等待与压缩期间的队列管理
//! （`queue_compaction_message` / `flush_compaction_queue`）。没有它，`events` 与
//! `commands` 子模块拿到数据后无处落地成可渲染的组件。
//!
//! # 与谁交互
//! 上游：`events` 在会话流事件到达时调用 `add_message_to_chat` /
//! `create_tool_component` / `show_status`；`session`、`commands`、`reload` 在会话切换、
//! 导航与重建时调用历史渲染。下游：本文件只向 `Container` 追加 `Component` 并调用
//! `InteractiveTuiReference::request_render`，不直接拼装终端转义序列。
//!
//! # 历史重建的执行流程
//! 1. `render_session_entries` 把每个 `SessionEntry` 展开成 `RenderSessionItem`：
//!    `custom` 与 `cache_warm` 的 usage 条目原样保留，其余条目经
//!    `session_entry_to_context_messages` 转成上下文消息；压缩 / 分支摘要若带 usage，
//!    在消息之后追加一条计费提示项。
//! 2. `render_session_items` 清空 `pending_tools`、按设置计算 cache miss，然后逐项追加
//!    组件：assistant 消息里的 toolCall 先建 `ToolExecutionComponent`，随后的 toolResult
//!    按 `tool_call_id` 回填；没有对应结果的 toolCall 留在 `pending_tools`，供后续流式
//!    事件继续更新。
//! 3. 末尾统一 `request_render`，由渲染器在下一帧读取组件树。
//!
//! # 重要约束
//! * 挂到容器里的组件统一用 `SharedComponentProxy` 包装（`super::shared` /
//!   `super::boxed_shared`），同一个 `Rc<RefCell<T>>` 因而既在渲染树里、又能被宿主继续
//!   修改。`show_status` 的去重与 `add_custom_entry_to_chat` 的插入位置都依赖这种
//!   “代理内部句柄指针相等”的判定，见 `shared_child_identity`。
//! * 渲染只做追加，不自动清理旧子项：需要整体替换时由调用方先 `clear()`
//!   （例如 `rebuild_chat_from_messages`）。因此重复调用 `show_status` 通常各追加一对
//!   Spacer+文本，只有紧邻的上一条状态行会被原地更新。
//! * `get_user_input` 与 `flush_compaction_queue` 在 `await` 前取出会话句柄或队列数据，
//!   绝不跨 `await` 持有 `inner` 的 `RefCell` 借用（同文件头借用纪律）。
//! * `fg` 与 `update_editor_border_color` 依赖全局主题已初始化；未初始化会 panic，
//!   与 TS 在相同位置抛错一致。
//!
//! # 用户输入与压缩队列的配套流程
//! 1. `get_user_input` 优先取 `pending_user_inputs` 队头；为空时安装 oneshot 回调，
//!    等 `commands` 的提交通路把文本送来（见 `commands/mod.rs` 的阶段 5）。
//! 2. 压缩进行中提交的消息经 `queue_compaction_message` 进入 `compaction_queued_messages`
//!    （按 Steer / FollowUp 标记），由 `update_pending_messages_display` 展示。
//! 3. 压缩结束后 `flush_compaction_queue` 把整批消息提交给会话；任何一条失败都经
//!    `restore_compaction_queue` 清空会话侧队列后整批放回压缩队列（不剔除已提交
//!    成功的条目），并显示错误。

use std::rc::Rc;

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, JsonValue, Message, StopReason, TextContent, TextKind, UserContent,
    UserContentBlock,
};
use kiana_core::changelog::LatestKianaRelease;
use kiana_core::tools_manager::{ToolStatus, ToolStatusKind};
use kiana_session::agent_session::parse_skill_block;
use kiana_session_core::cache_stats::{
    CACHE_TTL_MS, CacheMiss, ModelPriceInfo, ModelPriceSource, collect_cache_misses,
    detect_cache_miss,
};
use kiana_session_core::messages::{
    BashExecutionMessage, BranchSummaryMessage, CompactionSummaryMessage, CustomMessage,
};
use kiana_session_core::session_manager::{
    SessionEntry, UsageEntry, session_entry_to_context_messages,
};
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tools::truncate::TruncationResult;
use kiana_tui::components::markdown::Markdown;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::truncated_text::TruncatedText;
use kiana_tui::tui::Component;
use kiana_ui_kit::dynamic_border::DynamicBorder;
use kiana_ui_kit::keybinding_hints::key_display_text;
use kiana_ui_kit::themed_text::ThemedText;

use crate::components::assistant_message::AssistantMessageComponent;
use crate::components::bash_execution::BashExecutionComponent;
use crate::components::branch_summary_message::BranchSummaryMessageComponent;
use crate::components::compaction_summary_message::CompactionSummaryMessageComponent;
use crate::components::custom_entry::CustomEntryComponent;
use crate::components::custom_message::CustomMessageComponent;
use crate::components::footer::format_tokens;
use crate::components::skill_invocation_message::SkillInvocationMessageComponent;
use crate::components::tool_execution::{
    ToolExecutionComponent, ToolExecutionOptions, ToolExecutionResult,
};
use crate::components::user_message::UserMessageComponent;

use super::InteractiveMode;

/// `theme.fg(color, text)`（主题必须已初始化，否则 `expect` panic）。
///
/// 失败模式：全局主题未初始化时 `expect` panic；这是调用顺序错误，不是可恢复错误。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// `new Date().toISOString()`。
///
/// 固定 UTC、毫秒三位，输出形如 `2026-01-01T00:00:00.000Z`。当前文件内没有调用点
/// （`events` 子模块另有一份同名实现）；保留它是拆分协议的一部分。
fn iso_now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// 容器子项若是对 [`SharedComponentProxy`](crate::tui_renderer::SharedComponentProxy)
/// 的代理，返回其共享句柄的指针身份（对应 TS 的组件引用相等）。
///
/// 只对“代理”生效：未实现 `as_any` 的普通组件返回 `None`（本文件直接
/// `Box::new(Spacer)` / `Box::new(ThemedText)` 加入的子项就是这种情况）。因此需要事后
/// 按身份找回组件的调用点必须用 `super::shared` + `super::boxed_shared` 挂载，
/// 否则去重或定位会静默失效（不会报错，只是走“未命中”分支）。
/// 返回的是 `Rc<RefCell<T>>` 的堆地址；同一个句柄的克隆共享该地址。
fn shared_child_identity(component: &dyn Component) -> Option<usize> {
    let any = component.as_any()?;
    let proxy = any.downcast_ref::<crate::tui_renderer::SharedComponentProxy>()?;
    Some(std::rc::Rc::as_ptr(proxy.component()) as *const () as usize)
}

/// `addMessageToChat` 的选项。
///
/// 只在 `add_message_to_chat` 内部消费：`populate_history` 仅对 `user` 分支生效，
/// `message_id` / `timestamp` 只附着到用户消息与 assistant 消息组件上（其他角色
/// 忽略）。默认值表示“历史重建时不写编辑器历史、不带条目元数据”。
#[derive(Debug, Clone, Default)]
pub(crate) struct AddMessageOptions {
    /// 把用户消息加入编辑器历史（`renderInitialMessages` 的 `populateHistory`）。
    pub populate_history: bool,
    /// 会话条目 id（用户/assistant 消息右上角）。
    pub message_id: Option<String>,
    /// 会话条目时间戳。
    pub timestamp: Option<String>,
}

/// `renderSessionEntries` 的选项。
///
/// `update_footer` 决定重建前是否让 footer 失效并同步编辑器边框色；`populate_history`
/// 会透传给每条用户消息，把文本写进编辑器历史。两者默认 `false`，适合“把已有会话
/// 重新画出来”的场景。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RenderOptions {
    /// 重建后刷新 footer / 编辑器边框色。
    pub update_footer: bool,
    /// 用户消息加入编辑器历史。
    pub populate_history: bool,
}

/// `RenderSessionItem`：一条条目展开后的可渲染项。
///
/// 这是 `render_session_entries` 与 `render_session_items` 之间的中间表示：前者只做
/// “条目 → 项”的展开（读会话数据），后者只做“项 → 组件”的追加（写聊天容器）。拆开
/// 是为了在展开阶段一次性确定顺序，避免渲染阶段再次查询会话管理器。
pub(crate) enum RenderSessionItem {
    /// `custom` 或 `usage`（`cache_warm`）条目：原样保留。
    Entry(SessionEntry),
    /// 上下文消息（带来源条目元数据）。
    Message {
        /// 消息。
        message: AgentMessage,
        /// 来源条目 id。
        entry_id: Option<String>,
        /// 来源条目时间戳。
        timestamp: Option<String>,
    },
    /// 压缩 / 分支摘要的计费提示。
    CompactionCost {
        /// `"compaction"` 或 `"branch_summary"`。
        kind: String,
        /// 用量。
        usage: kiana_ai::types::Usage,
    },
}

/// [`ModelRuntime`](kiana_config::models::runtime::ModelRuntime) 的定价适配。
///
/// `cache_stats` 只需要目录里的 cache-read 单价；模型不在目录中时返回 `None`，
/// 由 `detect_cache_miss` 把读单价按 0 处理（见 `kiana-session-core::cache_stats`）。
/// 适配层存在的原因是 `ModelRuntime` 的查询方法名与返回类型与 `ModelPriceSource`
/// 的 trait 不同，不能直接实现。
struct RuntimePriceSource<'a>(&'a kiana_config::models::runtime::ModelRuntime);

impl ModelPriceSource for RuntimePriceSource<'_> {
    fn get_model(&self, provider: &str, model_id: &str) -> Option<ModelPriceInfo> {
        self.0
            .get_model(provider, model_id)
            .map(|model| ModelPriceInfo {
                cache_read: model.cost.cache_read,
            })
    }
}

/// `message.role` 的 assistant 视图（`AgentMessage::Standard(Assistant)`）。
///
/// 非标准消息或非 assistant 角色返回 `None`；调用方按 `None` 跳过而不是报错，
/// 因为历史里可能存在扩展写入的非标准消息。
fn assistant_of(message: &AgentMessage) -> Option<&kiana_ai::types::AssistantMessage> {
    message.as_assistant()
}

/// `getUserMessageText(message)`。
///
/// 只处理 `Message::User`：纯文本直接返回；内容块数组只拼接 `Text` 块（块之间不插
/// 分隔符），`Image` 块被丢弃。因此图片消息可能得到空串，调用方会据此跳过渲染。
/// 非 user 消息返回空串而不是报错。
pub(crate) fn user_message_text(message: &Message) -> String {
    let Message::User(user) = message else {
        return String::new();
    };
    match &user.content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Text(text) => Some(text.text.as_str()),
                UserContentBlock::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join(""),
    }
}

/// `countDroppedThinkingBlocks(message)`。
///
/// 遍历 `diagnostics`，只统计 `kind == "anthropic_input_transformations"` 且
/// `details.transformations` 是数组的条目，累加其中 `type == "thinking_dropped"` 的
/// 元素个数；没有 diagnostics、details 缺失或类型不符时按 0 计。多个诊断块的数量会
/// 相加，而不是取第一个。
fn count_dropped_thinking_blocks(message: &kiana_ai::types::AssistantMessage) -> usize {
    let Some(diagnostics) = &message.diagnostics else {
        return 0;
    };
    let mut count = 0usize;
    for diagnostic in diagnostics {
        if diagnostic.kind != "anthropic_input_transformations" {
            continue;
        }
        let Some(details) = &diagnostic.details else {
            continue;
        };
        let Some(JsonValue::Array(transformations)) = details.get("transformations") else {
            continue;
        };
        count += transformations
            .iter()
            .filter(|transformation| {
                transformation.get("type").and_then(JsonValue::as_str) == Some("thinking_dropped")
            })
            .count();
    }
    count
}

/// `kiana_ai` 的 `JsonObject`（`IndexMap`）→ `serde_json::Value`（`Map`）。
///
/// 逐项克隆，键顺序保持不变（workspace 的 `serde_json` 开启了 `preserve_order`）。
/// 转换后的值直接作为工具调用参数传给 `ToolExecutionComponent`；保留原顺序是为了让
/// 参数展示与来源 JSON 一致，不要换成无序 map 重建。
pub(crate) fn json_object_value(object: &kiana_ai::types::JsonObject) -> JsonValue {
    JsonValue::Object(
        object
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

/// 文本消息内容块（错误提示用）。
///
/// 用于把“操作被中止 / 出错”合成一条工具结果：`text_signature` 恒为 `None`，
/// 因为这是本地生成的提示，不是模型返回的签名文本。
fn text_block(text: &str) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_string(),
        text_signature: None,
    })
}

impl InteractiveMode {
    /// `this.ui.requestRender()`。
    ///
    /// 只标记“需要重绘”，实际绘制发生在渲染器下一次 `pump_events`；`false` 参数表示
    /// 不强制同步刷新。所有追加/修改组件的路径最后都应调用它。
    pub(crate) fn request_render(&mut self) {
        self.inner.borrow_mut().ui.request_render(false);
    }

    /// 组件/后台任务共用的重绘回调（弱引用，避免合成环）。
    ///
    /// 返回的闭包捕获 `Weak`，升级失败（模式已销毁）时静默不做事。组件把它存起来比存
    /// `InteractiveMode` 更安全：组件通常又被 `inner` 持有，强引用会形成
    /// `Rc` 循环、导致关停后内存不释放。
    pub(crate) fn request_render_callback(&self) -> Rc<dyn Fn()> {
        let weak = self.downgrade();
        Rc::new(move || {
            if let Some(inner) = weak.upgrade() {
                InteractiveMode { inner }
                    .inner
                    .borrow_mut()
                    .ui
                    .request_render(false);
            }
        })
    }

    /// 创建工具执行组件（`message_update` / `tool_execution_start` / 历史重建共用）。
    ///
    /// 读取当前设置里的 `show_images` / `image_width_cells`、`inner.output_pad`、
    /// 会话 cwd 与全局 `tool_output_expanded`，并查询扩展注册的工具定义；返回的组件
    /// 尚未挂到容器，调用方负责 `add_tool_component_to_chat` 或存入 `pending_tools`。
    /// 返回 `Rc<RefCell<..>>` 是因为同一条工具调用在流式期间会被多次更新。
    pub(crate) fn create_tool_component(
        &mut self,
        tool_name: &str,
        tool_call_id: &str,
        args: JsonValue,
    ) -> Rc<std::cell::RefCell<ToolExecutionComponent>> {
        let (show_images, image_width_cells, output_pad, cwd, expanded) = {
            let inner = self.inner.borrow();
            let settings = self.settings_manager();
            (
                settings.get_show_images(),
                settings.get_image_width_cells(),
                inner.output_pad,
                self.session_manager().get_cwd().to_string(),
                inner.tool_output_expanded,
            )
        };
        let definition = self.get_registered_tool_definition(tool_name);
        let request_render = self.request_render_callback();
        let mut component = ToolExecutionComponent::new(
            tool_name,
            tool_call_id,
            args,
            ToolExecutionOptions {
                show_images,
                image_width_cells,
                output_pad,
            },
            definition,
            request_render,
            cwd,
        );
        component.set_expanded(expanded);
        Rc::new(std::cell::RefCell::new(component))
    }

    /// 把工具组件挂到 chat（TS `chatContainer.addChild(component)`）。
    ///
    /// 挂载前先调用 `register_expandable`，把它登记进全局的可展开组件表（弱引用）；
    /// 之后批量切换展开状态时能作用到仍存活的组件。然后用 `boxed_shared` 包装成
    /// 渲染树子项。注意这里只追加、不加 Spacer，空行由调用方或消息本身的布局决定。
    pub(crate) fn add_tool_component_to_chat(
        &self,
        component: &Rc<std::cell::RefCell<ToolExecutionComponent>>,
    ) {
        crate::interactive_mode::extension_ui::widgets::register_expandable(
            component,
            ToolExecutionComponent::set_expanded,
        );
        let handle = super::shared(component);
        let container = Rc::clone(&self.inner.borrow().chat_container);
        container
            .borrow_mut()
            .add_child(super::boxed_shared(&handle));
    }

    /// `renderInitialMessages()`。
    ///
    /// 顺序是契约：先用 `build_context_entries()`（进入模型上下文的条目）重建聊天区；
    /// 再按需追加项目信任警告；最后用 `get_entries()`（全部持久化条目，含已压缩掉的）
    /// 统计压缩次数并对用户提示 `Session compacted N time(s)`。
    /// 压缩提示用 `show_status`，因此会参与状态行去重。
    pub(crate) fn render_initial_messages(&mut self) {
        let manager = self.session_manager();
        let entries = manager.build_context_entries();
        self.render_session_entries(
            &entries,
            RenderOptions {
                update_footer: true,
                populate_history: true,
            },
        );
        self.render_project_trust_warning_if_needed();

        let all_entries = manager.get_entries();
        let compaction_count = all_entries
            .iter()
            .filter(|entry| entry.kind() == "compaction")
            .count();
        if compaction_count > 0 {
            let times = if compaction_count == 1 {
                "1 time".to_string()
            } else {
                format!("{compaction_count} times")
            };
            self.show_status(&format!("Session compacted {times}"));
        }
    }

    /// `renderProjectTrustWarningIfNeeded()`。
    ///
    /// 两个条件同时成立才提示：项目尚未被信任，且 cwd 下存在需要信任的项目资源
    /// （由 `has_trust_requiring_project_resources` 检查）。聊天区非空时先插一个
    /// Spacer，再把警告文本（`warning` 色，左右各 1 列留白）追加到末尾。
    /// 文本里嵌入 `CONFIG_DIR_NAME`，提示用户用 `/trust` 保存决定并重启。
    pub(crate) fn render_project_trust_warning_if_needed(&mut self) {
        let settings_manager = self.settings_manager();
        let cwd = self.session_manager().get_cwd().to_string();
        if settings_manager.is_project_trusted()
            || !kiana_config::trust::manager::has_trust_requiring_project_resources(&cwd)
        {
            return;
        }
        let container = Rc::clone(&self.inner.borrow().chat_container);
        let mut chat = container.borrow_mut();
        if !chat.children.is_empty() {
            chat.add_child(Box::new(Spacer::new()));
        }
        chat.add_child(Box::new(ThemedText::with_padding(
            Box::new(|| {
                fg(
                    ThemeColor::Warning,
                    &format!(
                        "This project is not trusted. Project {} resources and packages are ignored. Use /trust to save a trust decision, then restart kiana.",
                        kiana_core::config::CONFIG_DIR_NAME
                    ),
                )
            }),
            1,
            0,
        )));
    }

    /// `renderSessionEntries(entries, options)`。
    ///
    /// 第一阶段只读数据、不碰组件：把 `entries` 逐条展开成 `RenderSessionItem`。
    /// * `custom` 条目与 `kind_name == "cache_warm"` 的 usage 条目原样保留（它们不经
    ///   上下文转换，直接由 `render_session_items` 特判渲染）。
    /// * 其余条目经 `session_entry_to_context_messages` 变成零到多条上下文消息，每条
    ///   都带上来源条目的 id / timestamp。
    /// * 压缩与分支摘要若有 usage 且确实产出了消息，额外追加一条 `CompactionCost`
    ///   项，从而保证计费提示排在摘要之后。
    ///
    /// 展开顺序就是最终渲染顺序。全屏模式下先 `reset_text_selection`：旧选区坐标指向
    /// 即将被替换的 transcript，保留下来会指向错的行。
    pub(crate) fn render_session_entries(
        &mut self,
        entries: &[SessionEntry],
        options: RenderOptions,
    ) {
        // 选区坐标指向被替换的 transcript（#9311）。
        {
            let renderer = Rc::clone(&self.inner.borrow().renderer);
            let mut renderer = renderer.borrow_mut();
            if renderer.mode() == kiana_tui::TuiMode::Fullscreen {
                renderer.reset_text_selection();
            }
        }

        let mut items: Vec<RenderSessionItem> = Vec::new();
        for entry in entries {
            let is_custom = entry.kind() == "custom";
            // `cache_warm` 有两种来源：正常反序列化出的 typed `Usage`，以及 typed 解析
            // 失败、只能落进 `Unknown` 的条目（`SessionEntry::Unknown` 保留原始对象）。
            // 后者要再看原始对象的 `kind` 字段，否则会被当成普通条目展开而丢掉这条用量。
            let is_cache_warm_usage = matches!(entry, SessionEntry::Usage(usage) if usage.kind_name == "cache_warm")
                || (entry.kind() == "usage"
                    && matches!(entry, SessionEntry::Unknown(object)
                        if object.get("kind").and_then(JsonValue::as_str) == Some("cache_warm")));
            if is_custom || is_cache_warm_usage {
                items.push(RenderSessionItem::Entry(entry.clone()));
                continue;
            }
            let messages = session_entry_to_context_messages(entry);
            let entry_id = Some(entry.id().to_string());
            let timestamp = entry.timestamp().map(str::to_string);
            let mut message_items: Vec<RenderSessionItem> = messages
                .into_iter()
                .map(|message| RenderSessionItem::Message {
                    message,
                    entry_id: entry_id.clone(),
                    timestamp: timestamp.clone(),
                })
                .collect();
            if (entry.kind() == "compaction" || entry.kind() == "branch_summary")
                && !message_items.is_empty()
            {
                let usage = match entry {
                    SessionEntry::Compaction(compaction) => compaction.usage.clone(),
                    SessionEntry::BranchSummary(summary) => summary.usage.clone(),
                    _ => None,
                };
                if let Some(usage) = usage {
                    message_items.push(RenderSessionItem::CompactionCost {
                        kind: entry.kind().to_string(),
                        usage,
                    });
                }
            }
            items.extend(message_items);
        }
        self.render_session_items(items, options);
    }

    /// `renderSessionItems(items, messageMeta?, options)`。
    ///
    /// 第二阶段：按 `items` 顺序把组件追加到 `chat_container`，并维护工具调用的配对
    /// 状态。关键行为与边界：
    /// * 开头清空 `pending_tools`：重建后只保留本次展开出的未完成工具调用，旧组件不
    ///   再被事件路径引用。
    /// * cache miss 只在设置开启时计算；`collect_cache_misses` 返回的是“传入 entries
    ///   切片的下标”，因此同一段代码里用同一份 `get_entries()` 结果建立
    ///   `下标 -> 条目 id` 映射，不能用调用方传入的 `entries`（可能是子集）。
    /// * assistant 消息先渲染自身，再为每个 toolCall 建组件；`stop_reason` 为
    ///   `Aborted` / `Error` 时把工具组件直接标成错误结果（历史里不会有后续
    ///   toolResult），其余情况暂存到 `rendered_pending`。
    /// * 随后的 `toolResult` 用 `tool_call_id` 从 `rendered_pending` 取出组件并回填
    ///   结果；取不到（没有对应 toolCall）时静默丢弃，不渲染孤立的工具结果。
    /// * 循环结束后仍未被认领的工具组件写回 `inner.pending_tools`，等待流式事件。
    ///
    /// 本函数只做追加，不清理现有子项；调用方需要“替换”语义时应先 `clear()`。
    pub(crate) fn render_session_items(
        &mut self,
        items: Vec<RenderSessionItem>,
        options: RenderOptions,
    ) {
        {
            let mut inner = self.inner.borrow_mut();
            inner.pending_tools.clear();
        }
        // cache miss 不持久化，必须每次重建时从 entry 列表重新推导。`collect_cache_misses`
        // 返回下标；条目 id 与 miss 都用同一份 `get_entries()` 的顺序配对，避免下标错位。
        let show_cache_miss_notices = self.settings_manager().get_show_cache_miss_notices();
        let cache_misses: Vec<(String, CacheMiss)> = if show_cache_miss_notices {
            let entry_ids: Vec<String> = self
                .session_manager()
                .get_entries()
                .iter()
                .map(|entry| entry.id().to_string())
                .collect();
            let session = self.session();
            let price_source = RuntimePriceSource(session.model_runtime());
            collect_cache_misses(&session_manager_entries(&session), &price_source)
                .into_iter()
                .filter_map(|(index, miss)| entry_ids.get(index).cloned().map(|id| (id, miss)))
                .collect()
        } else {
            Vec::new()
        };

        if options.update_footer {
            let footer = Rc::clone(&self.inner.borrow().footer);
            footer.borrow_mut().invalidate();
            self.update_editor_border_color();
        }

        let mut rendered_pending: indexmap::IndexMap<
            String,
            Rc<std::cell::RefCell<ToolExecutionComponent>>,
        > = indexmap::IndexMap::new();

        for item in items {
            match item {
                RenderSessionItem::Entry(entry) => {
                    if entry.kind() == "custom" {
                        if let SessionEntry::Custom(custom) = &entry {
                            self.add_custom_entry_to_chat(custom);
                        }
                    } else if let SessionEntry::Usage(usage) = &entry {
                        self.add_cache_warming_usage(usage);
                    }
                }
                RenderSessionItem::CompactionCost { kind, usage } => {
                    self.add_compaction_cost_notice(&kind, &usage);
                }
                RenderSessionItem::Message {
                    message,
                    entry_id,
                    timestamp,
                } => {
                    if message.role() == "assistant" {
                        self.add_message_to_chat(
                            &message,
                            AddMessageOptions {
                                populate_history: false,
                                message_id: entry_id.clone(),
                                timestamp: timestamp.clone(),
                            },
                        );
                        let stop_reason =
                            assistant_of(&message).map(|assistant| assistant.stop_reason);
                        let retry_attempt = self.session().retry_attempt();
                        if let Some(assistant) = assistant_of(&message) {
                            // 每个 toolCall 都建一个组件；错误/中止的轮次没有后续
                            // toolResult，所以在这里合成一条错误结果，否则组件会永远
                            // 停在“运行中”。正常轮次则先挂起，等 toolResult 回填。
                            for content in &assistant.content {
                                let AssistantContentBlock::ToolCall(call) = content else {
                                    continue;
                                };
                                let component = self.create_tool_component(
                                    &call.name,
                                    &call.id,
                                    json_object_value(&call.arguments),
                                );
                                self.add_tool_component_to_chat(&component);
                                match stop_reason {
                                    Some(StopReason::Aborted) => {
                                        let error_message = if retry_attempt > 0 {
                                            format!(
                                                "Aborted after {retry_attempt} retry attempt{}",
                                                if retry_attempt > 1 { "s" } else { "" }
                                            )
                                        } else {
                                            "Operation aborted".to_string()
                                        };
                                        component.borrow_mut().update_result(
                                            ToolExecutionResult::new(
                                                vec![text_block(&error_message)],
                                                JsonValue::Null,
                                                true,
                                            ),
                                            false,
                                        );
                                    }
                                    Some(StopReason::Error) => {
                                        let error_message = assistant
                                            .error_message
                                            .clone()
                                            .filter(|message| !message.is_empty())
                                            .unwrap_or_else(|| "Error".to_string());
                                        component.borrow_mut().update_result(
                                            ToolExecutionResult::new(
                                                vec![text_block(&error_message)],
                                                JsonValue::Null,
                                                true,
                                            ),
                                            false,
                                        );
                                    }
                                    _ => {
                                        rendered_pending.insert(call.id.clone(), component);
                                    }
                                }
                            }
                        }
                        // 失败轮次的 cache miss 没有参考意义，不提示。
                        let is_error_or_aborted = matches!(
                            stop_reason,
                            Some(StopReason::Aborted) | Some(StopReason::Error)
                        );
                        if !is_error_or_aborted
                            && let Some(entry_id) = entry_id
                            && let Some((_, miss)) =
                                cache_misses.iter().find(|(id, _)| id == &entry_id)
                        {
                            self.add_cache_miss_notice(miss);
                        }
                    // toolResult 只用来回填前面创建的组件：`shift_remove` 保证一个结果
                    // 只消费一次；找不到对应 toolCall（例如历史被裁剪过）时不渲染。
                    } else if message.role() == "toolResult"
                        && let Some(Message::ToolResult(result)) = message.as_standard()
                        && let Some(component) = rendered_pending.shift_remove(&result.tool_call_id)
                    {
                        let execution_result = ToolExecutionResult::new(
                            result.content.clone(),
                            result.details.clone().unwrap_or(JsonValue::Null),
                            result.is_error,
                        )
                        .with_duration_ms(result.duration_ms);
                        component
                            .borrow_mut()
                            .update_result(execution_result, false);
                    } else if message.role() != "toolResult" {
                        self.add_message_to_chat(
                            &message,
                            AddMessageOptions {
                                populate_history: options.populate_history,
                                message_id: entry_id,
                                timestamp,
                            },
                        );
                    }
                }
            }
        }

        // 未等到 toolResult 的工具调用写回共享状态，后续 `tool_execution_end` /
        // `message_end` 事件按 id 找到同一个组件继续更新。
        if !rendered_pending.is_empty() {
            let mut inner = self.inner.borrow_mut();
            for (tool_call_id, component) in rendered_pending {
                inner.pending_tools.insert(tool_call_id, component);
            }
        }
        self.request_render();
    }

    /// `addCustomEntryToChat(entry)`。
    ///
    /// 由扩展按 `entry.custom_type` 注册的 entry renderer 决定内容；没有对应 renderer
    /// 或组件渲染后没有内容（`has_content() == false`）时静默跳过，不产生空行。
    /// 若当前有流式 assistant 组件，则把新组件插到它**之前**（按共享句柄指针定位），
    /// 否则追加到末尾；这样流式内容始终留在最新位置。
    pub(crate) fn add_custom_entry_to_chat(
        &mut self,
        entry: &kiana_session_core::session_manager::CustomEntry,
    ) {
        let runner = self.session().extension_runner();
        let Some(renderer) = runner.get_entry_renderer(&entry.custom_type) else {
            return;
        };
        let output_pad = self.inner.borrow().output_pad;
        let tool_expanded = self.inner.borrow().tool_output_expanded;
        let mut component = CustomEntryComponent::new(entry.clone(), renderer, output_pad);
        component.set_expanded(tool_expanded);
        if !component.has_content() {
            return;
        }
        let component = Rc::new(std::cell::RefCell::new(component));
        crate::interactive_mode::extension_ui::widgets::register_expandable(
            &component,
            CustomEntryComponent::set_expanded,
        );
        let handle = super::shared(&component);
        let component: Box<dyn Component> = super::boxed_shared(&handle);
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        let streaming = self.inner.borrow().streaming_component.clone();
        let mut chat = chat.borrow_mut();
        if let Some(streaming) = streaming {
            let streaming_ptr = Rc::as_ptr(&streaming) as *const () as usize;
            if let Some(index) = chat
                .children
                .iter()
                .position(|child| shared_child_identity(child.as_ref()) == Some(streaming_ptr))
            {
                chat.children.insert(index, component);
                return;
            }
        }
        chat.add_child(component);
    }

    /// `addMessageToChat(message, options)`。
    ///
    /// 按 `message.role()` 字符串分派到不同组件，未知角色与 `system` / `toolResult`
    /// 一律跳过（`toolResult` 由前面 assistant 的 `ToolExecutionComponent` 承担）。
    /// 各分支的共同约定：
    /// * 消息类型转换失败（`from_agent_message` 返回 `None`）时静默返回，不部分渲染。
    /// * 需要与上下文分隔的块（user、compaction/branch summary、skill 调用）在聊天区
    ///   非空时先加 `Spacer`；assistant 与 bash/custom 消息不加。
    /// * `options.populate_history` 只对 `user` 分支生效：渲染后把原文加入编辑器历史。
    /// * `options.message_id` / `timestamp` 附着到用户与 assistant 消息组件，用于显示
    ///   条目元数据；其他分支忽略。
    pub(crate) fn add_message_to_chat(
        &mut self,
        message: &AgentMessage,
        options: AddMessageOptions,
    ) {
        // 先克隆容器句柄再按角色分派；各分支只追加自己的组件，不触碰其他分支的
        // 状态（除了下面注明的 Spacer / 编辑器历史约定）。
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        match message.role() {
            "bashExecution" => {
                let Some(bash) = BashExecutionMessage::from_agent_message(message) else {
                    return;
                };
                let output_pad = self.inner.borrow().output_pad;
                let mut component = BashExecutionComponent::new(
                    bash.command.clone(),
                    Some(self.request_render_callback()),
                    bash.exclude_from_context.unwrap_or(false),
                    output_pad,
                );
                if !bash.output.is_empty() {
                    component.append_output(&bash.output);
                }
                let truncation = bash.truncated.then(|| TruncationResult {
                    truncated: true,
                    ..Default::default()
                });
                component.set_complete(
                    bash.exit_code.map(|code| code as i32),
                    bash.cancelled,
                    truncation,
                    bash.full_output_path.clone(),
                );
                let component = Rc::new(std::cell::RefCell::new(component));
                crate::interactive_mode::extension_ui::widgets::register_expandable(
                    &component,
                    BashExecutionComponent::set_expanded,
                );
                let handle = super::shared(&component);
                chat.borrow_mut().add_child(super::boxed_shared(&handle));
            }
            "custom" => {
                let Some(custom) = CustomMessage::from_agent_message(message) else {
                    return;
                };
                if !custom.display {
                    return;
                }
                let runner = self.session().extension_runner();
                let renderer = runner.get_message_renderer(&custom.custom_type);
                let theme = self.get_markdown_theme_with_settings();
                let output_pad = self.inner.borrow().output_pad;
                let tool_expanded = self.inner.borrow().tool_output_expanded;
                let mut component =
                    CustomMessageComponent::new(custom, renderer, theme, output_pad);
                component.set_expanded(tool_expanded);
                let component = Rc::new(std::cell::RefCell::new(component));
                crate::interactive_mode::extension_ui::widgets::register_expandable(
                    &component,
                    CustomMessageComponent::set_expanded,
                );
                let handle = super::shared(&component);
                chat.borrow_mut().add_child(super::boxed_shared(&handle));
            }
            "compactionSummary" => {
                let Some(summary) = CompactionSummaryMessage::from_agent_message(message) else {
                    return;
                };
                let theme = self.get_markdown_theme_with_settings();
                let output_pad = self.inner.borrow().output_pad;
                let tool_expanded = self.inner.borrow().tool_output_expanded;
                let mut component =
                    CompactionSummaryMessageComponent::new(summary, theme, output_pad);
                component.set_expanded(tool_expanded);
                let component = Rc::new(std::cell::RefCell::new(component));
                crate::interactive_mode::extension_ui::widgets::register_expandable(
                    &component,
                    CompactionSummaryMessageComponent::set_expanded,
                );
                let handle = super::shared(&component);
                let mut chat = chat.borrow_mut();
                chat.add_child(Box::new(Spacer::new()));
                chat.add_child(super::boxed_shared(&handle));
            }
            "branchSummary" => {
                let Some(summary) = BranchSummaryMessage::from_agent_message(message) else {
                    return;
                };
                let theme = self.get_markdown_theme_with_settings();
                let output_pad = self.inner.borrow().output_pad;
                let tool_expanded = self.inner.borrow().tool_output_expanded;
                let mut component = BranchSummaryMessageComponent::new(summary, theme, output_pad);
                component.set_expanded(tool_expanded);
                let component = Rc::new(std::cell::RefCell::new(component));
                crate::interactive_mode::extension_ui::widgets::register_expandable(
                    &component,
                    BranchSummaryMessageComponent::set_expanded,
                );
                let handle = super::shared(&component);
                let mut chat = chat.borrow_mut();
                chat.add_child(Box::new(Spacer::new()));
                chat.add_child(super::boxed_shared(&handle));
            }
            "system" => {}
            "user" => {
                let text = match message.as_standard() {
                    Some(standard) => user_message_text(standard),
                    None => String::new(),
                };
                if text.is_empty() {
                    return;
                }
                let theme = self.get_markdown_theme_with_settings();
                let output_pad = self.inner.borrow().output_pad;
                let tool_expanded = self.inner.borrow().tool_output_expanded;
                let transformers = self.get_markdown_transformers();
                let skill_block = parse_skill_block(&text);
                if let Some(block) = skill_block {
                    // skill 调用渲染为独立组件；若该 skill 还携带用户提示文本
                    //（`user_message`），在其后追加一条普通用户消息组件。
                    let mut component = SkillInvocationMessageComponent::new(
                        block.clone(),
                        theme.clone(),
                        output_pad,
                    );
                    component.set_expanded(tool_expanded);
                    let component = Rc::new(std::cell::RefCell::new(component));
                    crate::interactive_mode::extension_ui::widgets::register_expandable(
                        &component,
                        SkillInvocationMessageComponent::set_expanded,
                    );
                    let handle = super::shared(&component);
                    let mut chat = chat.borrow_mut();
                    if !chat.children.is_empty() {
                        chat.add_child(Box::new(Spacer::new()));
                    }
                    chat.add_child(super::boxed_shared(&handle));
                    if let Some(user_message) = &block.user_message {
                        chat.add_child(Box::new(Spacer::new()));
                        chat.add_child(Box::new(UserMessageComponent::new(
                            user_message.clone(),
                            theme,
                            output_pad,
                            transformers,
                            options.message_id.clone(),
                            options.timestamp.clone(),
                        )));
                    }
                } else {
                    let component = UserMessageComponent::new(
                        text.clone(),
                        theme,
                        output_pad,
                        transformers,
                        options.message_id.clone(),
                        options.timestamp.clone(),
                    );
                    let mut chat = chat.borrow_mut();
                    if !chat.children.is_empty() {
                        chat.add_child(Box::new(Spacer::new()));
                    }
                    chat.add_child(Box::new(component));
                }
                if options.populate_history {
                    // TS: `this.editor.addToHistory?.(text)`。
                    self.inner.borrow().editor.add_to_history(&text);
                }
            }
            "assistant" => {
                let Some(assistant) = assistant_of(message) else {
                    return;
                };
                let (hide, theme, label, output_pad) = {
                    // thinking 块的隐藏开关与折叠标签来自 inner 的运行期状态
                    //（`/thinking` 等命令可改），不在构造时固定。
                    let inner = self.inner.borrow();
                    (
                        inner.hide_thinking_block,
                        self.get_markdown_theme_with_settings(),
                        inner.hidden_thinking_label.clone(),
                        inner.output_pad,
                    )
                };
                let transformers = self.get_markdown_transformers();
                let component = Rc::new(std::cell::RefCell::new(AssistantMessageComponent::new(
                    Some(assistant.clone()),
                    hide,
                    theme,
                    label,
                    output_pad,
                    transformers,
                    options.message_id,
                    options.timestamp,
                )));
                crate::interactive_mode::extension_ui::widgets::register_assistant_component(
                    &component,
                );
                let handle = super::shared(&component);
                chat.borrow_mut().add_child(super::boxed_shared(&handle));
            }
            "toolResult" => {}
            _ => {}
        }
    }

    /// `getMarkdownTransformers()`。
    ///
    /// 返回顺序固定：先内置的 mermaid 转换器，再扩展注册的转换器（后者按扩展 runner
    /// 给出的顺序）。顺序就是 markdown 渲染时管道的应用顺序。
    pub(crate) fn get_markdown_transformers(&self) -> Vec<kiana_ext_api::MarkdownTransformer> {
        let mut transformers = vec![self.inner.borrow().mermaid_markdown_transformer.clone()];
        let runner = self.session().extension_runner();
        transformers.extend(runner.get_markdown_transformers());
        transformers
    }

    /// `addCacheWarmingUsage(entry)`。
    ///
    /// 渲染一条 dim 色的 cache 预热用量行（文本来自 `format_cache_warming_usage`）。
    /// 设置关闭时整条跳过；与状态行不同，它没有去重，每次调用都追加一对
    /// Spacer+文本。
    pub(crate) fn add_cache_warming_usage(&mut self, entry: &UsageEntry) {
        if !self.settings_manager().get_show_cache_miss_notices() {
            return;
        }
        let usage = kiana_config::cache::warmer::format_cache_warming_usage(entry);
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        let mut chat = chat.borrow_mut();
        chat.add_child(Box::new(Spacer::new()));
        chat.add_child(Box::new(ThemedText::with_padding(
            Box::new(move || fg(ThemeColor::Dim, &usage)),
            1,
            0,
        )));
    }

    /// `addCompactionCostNotice(notice)`。
    ///
    /// 渲染压缩 / 分支摘要的计费提示（warning 色）。计费 token 数是四项之和
    /// （input + output + cache_read + cache_write）；费用仅在 `>= 0.01` 时显示
    /// （保留两位小数），更小的金额省略不写。设置关闭时整条跳过。
    pub(crate) fn add_compaction_cost_notice(
        &mut self,
        kind: &str,
        usage: &kiana_ai::types::Usage,
    ) {
        if !self.settings_manager().get_show_cache_miss_notices() {
            return;
        }
        let tokens = usage.input + usage.output + usage.cache_read + usage.cache_write;
        let cost = if usage.cost.total >= 0.01 {
            format!(
                " (~${})",
                kiana_ai::model_catalog::js_to_fixed(usage.cost.total, 2)
            )
        } else {
            String::new()
        };
        let label = if kind == "compaction" {
            "Compaction"
        } else {
            "Branch summary"
        };
        let line = format!("{label}: {} tokens billed{cost}", format_tokens(tokens));
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        let mut chat = chat.borrow_mut();
        chat.add_child(Box::new(Spacer::new()));
        chat.add_child(Box::new(ThemedText::with_padding(
            Box::new(move || fg(ThemeColor::Warning, &line)),
            1,
            0,
        )));
    }

    /// `maybeShowThinkingDropNotice(message)`。
    ///
    /// Anthropic 输入转换丢弃 thinking 块时提示。只在本次丢弃数**严格大于**分支里
    /// 上一条 assistant 消息的丢弃数时提示（向前数到最近一条 assistant 为止，没有
    /// 则按 0），避免同一批丢弃在重试 / 多轮时重复刷屏。设置关闭时跳过。
    pub(crate) fn maybe_show_thinking_drop_notice(
        &mut self,
        message: &kiana_ai::types::AssistantMessage,
    ) {
        if !self.settings_manager().get_show_cache_miss_notices() {
            return;
        }
        let dropped_count = count_dropped_thinking_blocks(message);
        if dropped_count == 0 {
            return;
        }
        let branch = self.session_manager().get_branch(None);
        let mut previous_dropped_count = 0usize;
        for entry in branch.iter().rev() {
            if let Some(AgentMessage::Standard(Message::Assistant(assistant))) = entry.message() {
                previous_dropped_count = count_dropped_thinking_blocks(assistant);
                break;
            }
        }
        if dropped_count <= previous_dropped_count {
            return;
        }
        let noun = if dropped_count == 1 {
            "thinking block"
        } else {
            "thinking blocks"
        };
        let line = format!("Anthropic dropped {dropped_count} {noun} (details in session)");
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        let mut chat = chat.borrow_mut();
        chat.add_child(Box::new(Spacer::new()));
        chat.add_child(Box::new(ThemedText::with_padding(
            Box::new(move || fg(ThemeColor::Warning, &line)),
            1,
            0,
        )));
    }

    /// `maybeShowCacheMissNotice(message)`。
    ///
    /// 对刚完成的 assistant 消息单独判定 cache miss（`detect_cache_miss` 的调用方
    /// 契约：`entries` 尚不包含该消息，时序由事件路径保证）。设置关闭时跳过；
    /// 判定出 miss 后仍由 `add_cache_miss_notice` 的阈值决定是否真的显示。
    pub(crate) fn maybe_show_cache_miss_notice(
        &mut self,
        message: &kiana_ai::types::AssistantMessage,
    ) {
        if !self.settings_manager().get_show_cache_miss_notices() {
            return;
        }
        let entries = self.session_manager().get_entries();
        let session = self.session();
        let price_source = RuntimePriceSource(session.model_runtime());
        if let Some(miss) = detect_cache_miss(&entries, message, &price_source) {
            self.add_cache_miss_notice(&miss);
        }
    }

    /// `addCacheMissNotice(miss)`。
    ///
    /// 阈值过滤：missed_tokens < 20000 且 missed_cost < 0.1 时不提示（噪声忽略）。
    /// 标签三选一：模型切换、空闲超过 `CACHE_TTL_MS`（5 分钟，向上取整到分钟）、
    /// 其余按普通 cache miss。费用 `>= 0.01` 才显示（两位小数）。没有去重。
    pub(crate) fn add_cache_miss_notice(&mut self, miss: &CacheMiss) {
        if miss.missed_tokens < 20_000.0 && miss.missed_cost < 0.1 {
            return;
        }
        let cost = if miss.missed_cost >= 0.01 {
            format!(
                " (~${})",
                kiana_ai::model_catalog::js_to_fixed(miss.missed_cost, 2)
            )
        } else {
            String::new()
        };
        let re_billed = format!(
            "{} tokens re-billed{cost}",
            format_tokens(miss.missed_tokens.max(0.0) as u64)
        );
        let label = if miss.model_changed {
            "Cache miss after model switch".to_string()
        } else if miss.idle_ms >= CACHE_TTL_MS {
            format!(
                "Cache miss after {}m idle",
                (miss.idle_ms / 60_000.0).round() as i64
            )
        } else {
            "Cache miss".to_string()
        };
        let line = format!("{label}: {re_billed}");
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        let mut chat = chat.borrow_mut();
        chat.add_child(Box::new(Spacer::new()));
        chat.add_child(Box::new(ThemedText::with_padding(
            Box::new(move || fg(ThemeColor::Warning, &line)),
            1,
            0,
        )));
    }

    /// `showManagedToolStatus(status)`。
    ///
    /// 管理型工具（如模型目录刷新）的状态行。与 `show_status` 不同：它不参与去重，
    /// 且第一条前补 Spacer、后续各条之间不补（用 `managed_tool_status_started` 标记）；
    /// 追加后主动清空 `last_status_spacer` / `last_status_text`，使下一次 `show_status`
    /// 不会误判“紧邻上一条还是状态行”而去原地更新。
    pub(crate) fn show_managed_tool_status(&mut self, status: ToolStatus) {
        let started = {
            let mut inner = self.inner.borrow_mut();
            let started = inner.managed_tool_status_started;
            inner.managed_tool_status_started = true;
            started
        };
        let message = if status.kind == ToolStatusKind::Warning {
            format!("Warning: {}", status.message)
        } else {
            status.message
        };
        let color = if status.kind == ToolStatusKind::Warning {
            ThemeColor::Warning
        } else {
            ThemeColor::Dim
        };
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        {
            let mut chat = chat.borrow_mut();
            if !started {
                chat.add_child(Box::new(Spacer::new()));
            }
            chat.add_child(Box::new(ThemedText::with_padding(
                Box::new(move || fg(color, &message)),
                1,
                0,
            )));
        }
        let mut inner = self.inner.borrow_mut();
        inner.last_status_spacer = None;
        inner.last_status_text = None;
        inner.ui.request_render(false);
    }

    /// `showStatus(message)`：连续同类状态只更新上一行（对象恒等去重）。
    ///
    /// 去重条件是四个指针身份同时成立：容器末两条正是上一次记录的文本与 Spacer，
    /// 且 `inner.last_status_text` / `last_status_spacer` 都还持有句柄。满足时只改
    /// `last_status_message` 并 invalidate 文本（渲染期闭包从 `inner` 读最新文案）；
    /// 否则追加新的 Spacer+文本对并记录句柄。中间只要插入过任何其他组件，身份
    /// 对不上，就回落到追加——不会把状态行更新到错误的位置。
    pub(crate) fn show_status(&mut self, message: &str) {
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        let (last_identity, second_last_identity) = {
            let chat = chat.borrow();
            let len = chat.children.len();
            let last = if len > 0 {
                shared_child_identity(chat.children[len - 1].as_ref())
            } else {
                None
            };
            let second_last = if len > 1 {
                shared_child_identity(chat.children[len - 2].as_ref())
            } else {
                None
            };
            (last, second_last)
        };
        let (status_text_ptr, status_spacer_ptr) = {
            let inner = self.inner.borrow();
            (
                inner
                    .last_status_text
                    .as_ref()
                    .map(|handle| Rc::as_ptr(handle) as *const () as usize),
                inner
                    .last_status_spacer
                    .as_ref()
                    .map(|handle| Rc::as_ptr(handle) as *const () as usize),
            )
        };
        if let (Some(last), Some(second_last), Some(text_ptr), Some(spacer_ptr)) = (
            last_identity,
            second_last_identity,
            status_text_ptr,
            status_spacer_ptr,
        ) && last == text_ptr
            && second_last == spacer_ptr
        {
            let text = Rc::clone(
                self.inner
                    .borrow()
                    .last_status_text
                    .as_ref()
                    .expect("checked"),
            );
            {
                let mut inner = self.inner.borrow_mut();
                inner.last_status_message = message.to_string();
            }
            text.borrow_mut().invalidate();
            self.request_render();
            return;
        }

        let text = Rc::new(std::cell::RefCell::new(ThemedText::with_padding(
            {
                // 文案不闭包进组件，而是渲染期从 `inner.last_status_message` 读：
                // 去重命中时只需改这一个字段并 invalidate，就能更新已有文本。
                let weak = self.downgrade();
                Box::new(move || {
                    let message = weak
                        .upgrade()
                        .map(|inner| inner.borrow().last_status_message.clone())
                        .unwrap_or_default();
                    fg(ThemeColor::Dim, &message)
                })
            },
            1,
            0,
        )));
        let spacer: super::SharedComponent = Rc::new(std::cell::RefCell::new(Spacer::new()));
        let text_handle: super::SharedComponent = text.clone();
        {
            let mut chat = chat.borrow_mut();
            chat.add_child(super::boxed_shared(&spacer));
            chat.add_child(super::boxed_shared(&text_handle));
        }
        {
            let mut inner = self.inner.borrow_mut();
            inner.last_status_message = message.to_string();
            inner.last_status_spacer = Some(spacer);
            inner.last_status_text = Some(text_handle);
        }
        self.request_render();
    }

    /// `showError(errorMessage)`：先 Spacer 再 `Error:` 文本。
    ///
    /// 错误文本的横向 padding 用的是 `output_pad`（随整体输出缩进设置变化），
    /// 与其他提示行固定的 `1` 不同；无去重，每次调用各追加一对。
    pub(crate) fn show_error(&mut self, error_message: &str) {
        let line = format!("Error: {error_message}");
        let output_pad = self.inner.borrow().output_pad;
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        {
            let mut chat = chat.borrow_mut();
            chat.add_child(Box::new(Spacer::new()));
            chat.add_child(Box::new(ThemedText::with_padding(
                Box::new(move || fg(ThemeColor::Error, &line)),
                output_pad,
                0,
            )));
        }
        self.request_render();
    }

    /// `showWarning(warningMessage)`：`warning` 色、`paddingX = 1`。
    pub(crate) fn show_warning(&mut self, warning_message: &str) {
        let line = format!("Warning: {warning_message}");
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        {
            let mut chat = chat.borrow_mut();
            chat.add_child(Box::new(Spacer::new()));
            chat.add_child(Box::new(ThemedText::with_padding(
                Box::new(move || fg(ThemeColor::Warning, &line)),
                1,
                0,
            )));
        }
        self.request_render();
    }

    /// `showNewVersionNotification(release)`。
    ///
    /// 追加一个 DynamicBorder 包住的更新提示块：标题行、`kiana update` 指引、可选的
    /// release note（trim 后非空才渲染，markdown 样式）、Changelog 链接（终端支持
    /// OSC 8 超链接时输出可点击链接，未配置 changelog URL 时显示 "not configured"）、
    /// 收尾边框。无去重。
    pub(crate) fn show_new_version_notification(&mut self, release: LatestKianaRelease) {
        let version = release.version.clone();
        let note = release
            .note
            .clone()
            .map(|note| note.trim().to_string())
            .filter(|note| !note.is_empty());
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        let markdown_theme = self.get_markdown_theme_with_settings();
        {
            let mut chat = chat.borrow_mut();
            chat.add_child(Box::new(Spacer::new()));
            chat.add_child(Box::new(DynamicBorder::new(std::sync::Arc::new(
                |text: &str| fg(ThemeColor::Warning, text),
            ))));
            chat.add_child(Box::new(ThemedText::with_padding(
                Box::new(move || {
                    format!(
                        "{}\n{}{}",
                        super::bold(&fg(ThemeColor::Warning, "Update Available")),
                        fg(
                            ThemeColor::Muted,
                            &format!("New version {version} is available. Run ")
                        ),
                        fg(
                            ThemeColor::Accent,
                            &format!("{} update", kiana_core::config::APP_NAME)
                        )
                    )
                }),
                1,
                0,
            )));
            if let Some(note) = note {
                chat.add_child(Box::new(Spacer::new()));
                chat.add_child(Box::new(
                    Markdown::new(note, 1, 0, markdown_theme).with_default_text_style(
                        kiana_tui::components::markdown::DefaultTextStyle {
                            color: Some(std::sync::Arc::new(|text: &str| {
                                fg(ThemeColor::Muted, text)
                            })),
                            ..Default::default()
                        },
                    ),
                ));
                chat.add_child(Box::new(Spacer::new()));
            }
            chat.add_child(Box::new(ThemedText::with_padding(
                Box::new(|| {
                    let link = match kiana_core::config::get_api_url("changelog") {
                        Some(url) => {
                            if kiana_tui::terminal_image::get_capabilities().hyperlinks {
                                kiana_tui::terminal_image::hyperlink(
                                    &fg(ThemeColor::Accent, &url),
                                    &url,
                                )
                            } else {
                                fg(ThemeColor::Accent, &url)
                            }
                        }
                        None => fg(ThemeColor::Muted, "not configured"),
                    };
                    format!("{}{link}", fg(ThemeColor::Muted, "Changelog: "))
                }),
                1,
                0,
            )));
            chat.add_child(Box::new(DynamicBorder::new(std::sync::Arc::new(
                |text: &str| fg(ThemeColor::Warning, text),
            ))));
        }
        self.request_render();
    }

    /// `showPackageUpdateNotification(packages)`。
    ///
    /// 与版本通知同构的 DynamicBorder 块：标题、`kiana update --extensions` 指引、
    /// 逐行列出的包名。无去重。
    pub(crate) fn show_package_update_notification(&mut self, packages: Vec<String>) {
        let package_lines = packages
            .iter()
            .map(|package| format!("- {package}"))
            .collect::<Vec<_>>()
            .join("\n");
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        {
            let mut chat = chat.borrow_mut();
            chat.add_child(Box::new(Spacer::new()));
            chat.add_child(Box::new(DynamicBorder::new(std::sync::Arc::new(
                |text: &str| fg(ThemeColor::Warning, text),
            ))));
            chat.add_child(Box::new(ThemedText::with_padding(
                Box::new(move || {
                    format!(
                        "{}\n{}{}\n{}\n{}",
                        super::bold(&fg(ThemeColor::Warning, "Package Updates Available")),
                        fg(ThemeColor::Muted, "Package updates are available. Run "),
                        fg(
                            ThemeColor::Accent,
                            &format!("{} update --extensions", kiana_core::config::APP_NAME)
                        ),
                        fg(ThemeColor::Muted, "Packages:"),
                        package_lines
                    )
                }),
                1,
                0,
            )));
            chat.add_child(Box::new(DynamicBorder::new(std::sync::Arc::new(
                |text: &str| fg(ThemeColor::Warning, text),
            ))));
        }
        self.request_render();
    }

    /// `ensurePngTranscoder()`。
    ///
    /// 尝试注册 Kitty 图片转码器；`on_registered` 回调只在**本次调用完成注册**时
    /// 执行（见 `kiana_core::image_process::ensure_png_transcoder`），作用是重绘此前
    /// 以文本兜底显示的图片。回调经弱引用升级回 `InteractiveMode`：转码器注册是
    /// 线程本地状态，不持强引用，避免延长模式生命周期。
    pub(crate) fn ensure_png_transcoder(&mut self) {
        let weak = self.downgrade();
        kiana_core::image_process::ensure_png_transcoder(move || {
            if let Some(inner) = weak.upgrade() {
                let mode = InteractiveMode { inner };
                mode.inner.borrow_mut().ui.invalidate();
                mode.inner.borrow_mut().ui.request_render(false);
            }
        });
    }

    /// `updateEditorBorderColor()`：bash 模式用 bash 边框色，否则跟随 thinking level。
    ///
    /// 颜色要写到两处：全局可展开控件表里的编辑器边框样式函数（状态指示器渲染期
    /// 读取它），以及当前活动编辑器自身的边框。二者必须同步，否则状态指示器与
    /// 编辑器边框会出现不一致的颜色。主题未初始化时 panic（与 `fg` 同一前提）。
    pub(crate) fn update_editor_border_color(&mut self) {
        let is_bash_mode = self.inner.borrow().is_bash_mode;
        let color: kiana_tui::components::text_component::StyleFn = if is_bash_mode {
            let theme = kiana_theme::THEME
                .current()
                .expect("theme must be initialized before rendering (TS throws here)");
            theme.get_bash_mode_border_color().into()
        } else {
            let level = super::thinking_level_literal(self.session().thinking_level());
            let theme = kiana_theme::THEME
                .current()
                .expect("theme must be initialized before rendering (TS throws here)");
            theme.get_thinking_border_color(level).into()
        };
        // 状态指示器在渲染期读取同一个函数（TS `this.editor.borderColor`）。
        crate::interactive_mode::extension_ui::widgets::set_editor_border_color_style(Some(
            std::sync::Arc::clone(&color),
        ));
        self.inner
            .borrow()
            .editor
            .set_border_color(Some(Box::new(move |text: &str| color(text))));
        if let Some(indicator) = &mut self.inner.borrow_mut().active_status_indicator {
            indicator.invalidate();
        }
        self.request_render();
    }

    /// `getUserInput()`。
    ///
    /// 交互模式取下一条用户输入的统一入口。优先消费 `pending_user_inputs` 队头
    /// （提交通路在没人等待时先入队）；为空则创建 oneshot 通道，把发送端挂到
    /// `inner.on_input_callback`——`commands` 的阶段 5 会 `take()` 它并投递文本。
    /// 回调只发送一次（`sender.take()`），重复提交的文本走 `pending_user_inputs`。
    /// 若发送端在等待期间被丢弃（如模式销毁），`await` 返回 `Err`，此处按空串
    /// 处理而不是 panic。
    pub(crate) async fn get_user_input(&mut self) -> String {
        let queued = {
            let mut inner = self.inner.borrow_mut();
            if inner.pending_user_inputs.is_empty() {
                None
            } else {
                Some(inner.pending_user_inputs.remove(0))
            }
        };
        if let Some(queued) = queued {
            return queued;
        }
        let (sender, receiver) = tokio::sync::oneshot::channel::<String>();
        let mut sender = Some(sender);
        self.inner.borrow_mut().on_input_callback = Some(Box::new(move |text: &str| {
            if let Some(sender) = sender.take() {
                let _ = sender.send(text.to_string());
            }
        }));
        receiver.await.unwrap_or_default()
    }

    /// `clearEditor()`。
    pub(crate) fn clear_editor(&mut self) {
        self.inner.borrow().editor.set_text("");
        self.request_render();
    }

    /// `rebuildChatFromMessages()`。
    ///
    /// “整体替换”语义的实现样板：先 `clear()` 聊天容器，再按上下文条目重建。
    /// 用默认 `RenderOptions`（不刷 footer、不写编辑器历史），因为历史重放不应
    /// 重复产生这两类副作用。
    pub(crate) fn rebuild_chat_from_messages(&mut self) {
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        chat.borrow_mut().clear();
        let entries = self.session_manager().build_context_entries();
        self.render_session_entries(&entries, RenderOptions::default());
    }

    // -----------------------------------------------------------------------
    // 队列（TS 4683–4853）
    //
    // 压缩（compaction）进行中的用户输入不能直接交给会话，先按 Steer / FollowUp
    // 分两类存进 `inner.compaction_queued_messages`，展示在独立容器里。压缩结束后
    // `flush_compaction_queue` 按原顺序重放。队列的“全部内容”由两部分拼成：会话侧
    // 的 steering / follow-up 队列 + 本地压缩队列；读取与清空都同时覆盖两者。
    // -----------------------------------------------------------------------

    /// `getAllQueuedMessages()`。
    ///
    /// 只读快照：先拷贝会话侧的 steering / follow-up 队列，再把本地压缩队列按
    /// `mode` 归入两类。顺序即未来的提交顺序（会话侧在前，压缩队列在后）。
    pub(crate) fn get_all_queued_messages(&self) -> (Vec<String>, Vec<String>) {
        let session = self.session();
        let mut steering = session.get_steering_messages();
        let mut follow_up = session.get_follow_up_messages();
        let inner = self.inner.borrow();
        for message in &inner.compaction_queued_messages {
            match message.mode {
                super::CompactionQueueMode::Steer => steering.push(message.text.clone()),
                super::CompactionQueueMode::FollowUp => follow_up.push(message.text.clone()),
            }
        }
        (steering, follow_up)
    }

    /// `clearAllQueues()`。
    ///
    /// [`get_all_queued_messages`](Self::get_all_queued_messages) 的破坏性版本：同时清掉会话侧队列（`clear_queue`
    /// 返回被清出的文本）与本地压缩队列（`mem::take`），把两部分按同样的归类合并
    /// 返回，供“整批取回”的调用方（如恢复到编辑器）使用。
    pub(crate) fn clear_all_queues(&mut self) -> (Vec<String>, Vec<String>) {
        let session = self.session();
        let (steering, follow_up) = session.clear_queue();
        let queued = {
            let mut inner = self.inner.borrow_mut();
            std::mem::take(&mut inner.compaction_queued_messages)
        };
        let mut steering_result = steering;
        let mut follow_up_result = follow_up;
        for message in queued {
            match message.mode {
                super::CompactionQueueMode::Steer => steering_result.push(message.text),
                super::CompactionQueueMode::FollowUp => follow_up_result.push(message.text),
            }
        }
        (steering_result, follow_up_result)
    }

    /// `updatePendingMessagesDisplay()`。
    ///
    /// 整体重绘排队消息展示容器（先 `clear()`）：Steering 每条一行、Follow-up 每条
    /// 一行（均 dim 色、可截断），队列全空时容器保持为空。末尾附加一条出队快捷键
    /// 提示（文案来自按键映射 `app.message.dequeue`）。
    pub(crate) fn update_pending_messages_display(&mut self) {
        let container = Rc::clone(&self.inner.borrow().pending_messages_container);
        let (steering, follow_up) = self.get_all_queued_messages();
        // 第 12 周渲染核心（实验开关）：尚未回显的乐观用户消息与队列一起展示。
        let pending_users = self.pending_user_lines();
        let mut container = container.borrow_mut();
        container.clear();
        if steering.is_empty() && follow_up.is_empty() && pending_users.is_empty() {
            return;
        }
        container.add_child(Box::new(Spacer::new()));
        for message in &steering {
            let text = fg(ThemeColor::Dim, &format!("Steering: {message}"));
            container.add_child(Box::new(TruncatedText::with_padding(text, 1, 0)));
        }
        for message in &follow_up {
            let text = fg(ThemeColor::Dim, &format!("Follow-up: {message}"));
            container.add_child(Box::new(TruncatedText::with_padding(text, 1, 0)));
        }
        for message in &pending_users {
            let text = fg(ThemeColor::Dim, message);
            container.add_child(Box::new(TruncatedText::with_padding(text, 1, 0)));
        }
        let dequeue_hint = key_display_text("app.message.dequeue");
        let hint = fg(
            ThemeColor::Dim,
            &format!("↳ {dequeue_hint} to edit all queued messages"),
        );
        container.add_child(Box::new(TruncatedText::with_padding(hint, 1, 0)));
    }

    /// `restoreQueuedMessagesToEditor(options?)`。
    ///
    /// 清空全部队列，把所有排队文本（以空行连接）与编辑器现有文本合并写回编辑器，
    /// 返回取出的排队条数（含会话侧）。`abort` 为真时额外 `spawn_local` 会话中止
    /// （不等中止完成）。合并时跳过空串段；全空则只刷新展示容器。
    pub(crate) fn restore_queued_messages_to_editor(
        &mut self,
        abort: bool,
        current_text: Option<String>,
    ) -> usize {
        let (steering, follow_up) = self.clear_all_queues();
        let mut all_queued: Vec<String> = steering;
        all_queued.extend(follow_up);
        if all_queued.is_empty() {
            // 没有排队内容时仍要刷新展示（清掉残余显示），abort 语义照常生效。
            self.update_pending_messages_display();
            if abort {
                let session = self.session();
                tokio::task::spawn_local(async move { session.abort().await });
            }
            return 0;
        }
        let queued_text = all_queued.join("\n\n");
        let current_text = current_text.unwrap_or_else(|| self.inner.borrow().editor.get_text());
        let combined: Vec<String> = [queued_text, current_text]
            .into_iter()
            .filter(|text| !text.trim().is_empty())
            .collect();
        self.inner.borrow().editor.set_text(&combined.join("\n\n"));
        self.update_pending_messages_display();
        if abort {
            let session = self.session();
            tokio::task::spawn_local(async move { session.abort().await });
        }
        all_queued.len()
    }

    /// `queueCompactionMessage(text, mode)`。
    ///
    /// 压缩期间的用户输入入口：入本地压缩队列、写入编辑器历史、清空编辑器、刷新
    /// 展示容器，并用 `show_status` 提示（因此参与状态行去重）。
    pub(crate) fn queue_compaction_message(
        &mut self,
        text: &str,
        mode: super::CompactionQueueMode,
    ) {
        self.inner
            .borrow_mut()
            .compaction_queued_messages
            .push(super::CompactionQueuedMessage {
                text: text.to_string(),
                mode,
            });
        // TS: `this.editor.addToHistory?.(text)`（活动编辑器，可能是扩展编辑器）。
        self.inner.borrow().editor.add_to_history(text);
        self.inner.borrow().editor.set_text("");
        self.update_pending_messages_display();
        self.show_status("Queued message for after compaction");
    }

    /// `isExtensionCommand(text)`。
    ///
    /// 形如 `/name`（`/name ...` 取首个空格前的名字）且扩展 runner 注册了该命令。
    /// 用于 `flush_compaction_queue` 分流：扩展命令走 `prompt`（它内部会执行命令），
    /// 普通消息按 Steer / FollowUp 走对应队列方法。
    pub(crate) fn is_extension_command(&self, text: &str) -> bool {
        if !text.starts_with('/') {
            return false;
        }
        let command_name = match text.find(' ') {
            Some(index) => &text[1..index],
            None => &text[1..],
        };
        self.session()
            .extension_runner()
            .get_command(command_name)
            .is_some()
    }

    /// `flushCompactionQueue(options?)`。
    ///
    /// 压缩结束后重放排队消息。`will_retry` 是调用方（压缩结束事件）给出的重试
    /// 标记，决定走哪条路径：
    ///
    /// * `will_retry == true`（压缩将自动重试）：逐条提交并**等待每次调用完成**
    ///   （扩展命令走 `prompt`，普通消息按 `mode` 走 `steer` / `follow_up`），
    ///   失败即整批放回。
    /// * `will_retry == false`：先逐条 `prompt` 提交排在队头的扩展命令；找到第一条
    ///   普通消息后，以对应 `StreamingBehavior` **后台启动**它（`spawn_local` 不等待，
    ///   失败经弱引用恢复队列），随后 `yield_now` 一次让该 run 进入活动状态；其余
    ///   普通消息再按 steer/followUp 逐条提交并等待——run 已活动，这些调用会排进
    ///   当前 run 的 steer/follow-up 队列。队列里全是扩展命令时全部逐条 `prompt`。
    ///
    /// 任何一条提交失败都调用 `restore_compaction_queue` 整批放回并显示错误。
    /// 无论走哪条路径，函数入口都已 `take` 走整个本地队列；失败放回的是**原始
    /// 整批**（含此前已成功提交的条目）。
    pub(crate) async fn flush_compaction_queue(&mut self, will_retry: bool) {
        let queued = {
            let mut inner = self.inner.borrow_mut();
            if inner.compaction_queued_messages.is_empty() {
                return;
            }
            std::mem::take(&mut inner.compaction_queued_messages)
        };
        self.update_pending_messages_display();

        let restore_queue = |mode: &mut InteractiveMode,
                             queued: &[super::CompactionQueuedMessage],
                             error: String| {
            restore_compaction_queue(mode, queued, error);
        };

        if will_retry {
            // 重试路径：不启动“后台首条 + 排队余下”的机制，逐条同步等待；压缩重试
            // 结束后 run 仍是空闲的，逐条提交语义最简单、失败定位也最直接。
            for message in &queued {
                let session = self.session();
                let result = if self.is_extension_command(&message.text) {
                    session
                        .prompt(message.text.clone(), Default::default())
                        .await
                } else if message.mode == super::CompactionQueueMode::FollowUp {
                    session
                        .follow_up(message.text.clone(), None, None)
                        .await
                        .map(|_| ())
                } else {
                    session
                        .steer(message.text.clone(), None, None)
                        .await
                        .map(|_| ())
                };
                if let Err(error) = result {
                    restore_queue(self, &queued, error.to_string());
                    return;
                }
            }
            self.update_pending_messages_display();
            return;
        }

        let first_prompt_index = queued
            .iter()
            .position(|message| !self.is_extension_command(&message.text));
        let Some(first_prompt_index) = first_prompt_index else {
            // 队列里全是扩展命令：没有可后台启动的普通消息，全部逐条 prompt。
            for message in &queued {
                let session = self.session();
                if let Err(error) = session
                    .prompt(message.text.clone(), Default::default())
                    .await
                {
                    restore_queue(self, &queued, error.to_string());
                    return;
                }
            }
            return;
        };

        for message in &queued[..first_prompt_index] {
            // 第一条普通消息之前的扩展命令先逐条同步执行完，保持与队列一致的顺序。
            let session = self.session();
            if let Err(error) = session
                .prompt(message.text.clone(), Default::default())
                .await
            {
                restore_queue(self, &queued, error.to_string());
                return;
            }
        }

        let first = &queued[first_prompt_index];
        let rest = queued[first_prompt_index + 1..].to_vec();
        let streaming_behavior = match first.mode {
            super::CompactionQueueMode::Steer => kiana_ext_api::events::StreamingBehavior::Steer,
            super::CompactionQueueMode::FollowUp => {
                kiana_ext_api::events::StreamingBehavior::FollowUp
            }
        };
        // TS: `const promptPromise = this.session.prompt(...).catch(restoreQueue)`，**不等待**
        // 首个 prompt 完成，随后立即把剩余消息排进仍在运行的 run。Rust 的
        // `AgentSession::prompt` 只在整个 run 结束后返回，因此放到 LocalSet 任务里启动
        // （失败时通过弱引用回到 UI 线程恢复队列）。
        {
            let session = self.session();
            let first_text = first.text.clone();
            let queued_for_restore = queued.clone();
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                let result = session
                    .prompt(
                        first_text,
                        kiana_session::agent_session::PromptOptions {
                            streaming_behavior: Some(streaming_behavior),
                            ..Default::default()
                        },
                    )
                    .await;
                if let Err(error) = result
                    && let Some(inner) = weak.upgrade()
                {
                    let mut mode = InteractiveMode { inner };
                    restore_compaction_queue(&mut mode, &queued_for_restore, error.to_string());
                }
            });
        }
        // TS 的 prompt() 在返回 Promise 前已同步启动 run；spawn_local 需要一次让出才能开始
        // 执行，这里让首轮先进入 active 状态，剩余消息才会按 steer/followUp 排队。
        tokio::task::yield_now().await;

        for message in &rest {
            // run 已在后台启动（见 yield_now 注释），这里的 steer/followUp 会被
            // 会话侧排进当前 run 的队列，而不是再开新 run。
            let session = self.session();
            let result = if self.is_extension_command(&message.text) {
                session
                    .prompt(message.text.clone(), Default::default())
                    .await
            } else if message.mode == super::CompactionQueueMode::FollowUp {
                session
                    .follow_up(message.text.clone(), None, None)
                    .await
                    .map(|_| ())
            } else {
                session
                    .steer(message.text.clone(), None, None)
                    .await
                    .map(|_| ())
            };
            if let Err(error) = result {
                restore_queue(self, &queued, error.to_string());
                return;
            }
        }
        self.update_pending_messages_display();
    }

    /// `flushPendingBashComponents()`。
    ///
    /// 把 bash 执行期间暂存在 `pending_bash_components` 的组件从 pending 展示容器
    /// 挪到聊天容器：先按共享句柄指针身份在 pending 容器里找到并移除，再以
    /// `shared` + `boxed_shared` 挂到聊天容器末尾。找不到（已被其他路径移走）就
    /// 只做追加，不报错。
    pub(crate) fn flush_pending_bash_components(&mut self) {
        let pending: Vec<Rc<std::cell::RefCell<BashExecutionComponent>>> = {
            let mut inner = self.inner.borrow_mut();
            std::mem::take(&mut inner.pending_bash_components)
        };
        let pending_container = Rc::clone(&self.inner.borrow().pending_messages_container);
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        for component in pending {
            let ptr = Rc::as_ptr(&component) as *const () as usize;
            {
                let mut container = pending_container.borrow_mut();
                if let Some(index) = container
                    .children
                    .iter()
                    .position(|child| shared_child_identity(child.as_ref()) == Some(ptr))
                {
                    container.remove_child_at(index);
                }
            }
            let handle = super::shared(&component);
            chat.borrow_mut().add_child(super::boxed_shared(&handle));
        }
    }
}

/// `sessionManager.getEntries()` 的便捷封装（避免在 `InteractiveMode` 方法内多次借用）。
///
/// `render_session_items` 里 cache miss 的“下标 → 条目 id”映射与 `collect_cache_misses`
/// 都吃条目切片；这个自由函数让两处拿到同一份顺序，避免下标错位。
fn session_manager_entries(
    session: &kiana_session::agent_session::AgentSession,
) -> Vec<SessionEntry> {
    session.session_manager().get_entries()
}

/// `flushCompactionQueue` 的 `restoreQueue(error)`（失败时把整批消息放回队列并报错）。
///
/// 失败恢复的完整动作：清空会话侧队列（防止半提交状态残留）、把原始整批写回本地
/// 压缩队列、刷新展示容器、显示错误（复数处理）。不区分批次里哪条成功过——调用
/// 方（`flush_compaction_queue`）持有的是提交前的原始切片。
fn restore_compaction_queue(
    mode: &mut InteractiveMode,
    queued: &[super::CompactionQueuedMessage],
    error: String,
) {
    mode.session().clear_queue();
    mode.inner.borrow_mut().compaction_queued_messages = queued.to_vec();
    mode.update_pending_messages_display();
    let plural = if queued.len() > 1 { "s" } else { "" };
    mode.show_error(&format!("Failed to send queued message{plural}: {error}"));
}

#[doc(hidden)]
impl InteractiveMode {
    /// 向 `pendingUserInputs` 压入一条输入（测试用）。
    pub fn test_queue_user_input(&mut self, text: &str) {
        self.inner
            .borrow_mut()
            .pending_user_inputs
            .push(text.to_string());
    }

    /// 排队输入数量（测试用）。
    pub fn test_pending_user_input_count(&self) -> usize {
        self.inner.borrow().pending_user_inputs.len()
    }

    /// `getUserInput()` 的测试接缝。
    pub async fn test_get_user_input(&mut self) -> String {
        self.get_user_input().await
    }

    /// 把工具组件挂到 chat（测试用；`addToolComponentToChat` 的接缝）。
    pub fn test_add_tool_component_to_chat(
        &self,
        component: &Rc<
            std::cell::RefCell<crate::components::tool_execution::ToolExecutionComponent>,
        >,
    ) {
        self.add_tool_component_to_chat(component);
    }

    /// `queueCompactionMessage(text, mode)` 的测试接缝。
    pub fn test_queue_compaction_message(&mut self, text: &str, follow_up: bool) {
        self.queue_compaction_message(
            text,
            if follow_up {
                super::CompactionQueueMode::FollowUp
            } else {
                super::CompactionQueueMode::Steer
            },
        );
    }

    /// `flushCompactionQueue(willRetry)` 的测试接缝。
    pub async fn test_flush_compaction_queue(&mut self, will_retry: bool) {
        self.flush_compaction_queue(will_retry).await;
    }

    /// 当前排队的压缩消息文本。
    pub fn test_compaction_queue_texts(&self) -> Vec<String> {
        self.inner
            .borrow()
            .compaction_queued_messages
            .iter()
            .map(|message| message.text.clone())
            .collect()
    }

    /// 调用 `addCompactionCostNotice`（测试用）。
    pub fn test_add_compaction_cost(&mut self, tokens: u64, total_cost: f64) {
        self.test_add_compaction_cost_kind("compaction", tokens, total_cost);
    }

    /// 指定 `kind` 的 `addCompactionCostNotice`（测试用）。
    pub fn test_add_compaction_cost_kind(&mut self, kind: &str, tokens: u64, total_cost: f64) {
        let usage = kiana_ai::types::Usage {
            input: tokens,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            total_tokens: tokens,
            cost: kiana_ai::types::UsageCost {
                total: total_cost,
                ..Default::default()
            },
            ..Default::default()
        };
        self.add_compaction_cost_notice(kind, &usage);
    }
}
