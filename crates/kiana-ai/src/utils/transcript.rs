//! `utils/transcript.ts`——会话记录（transcript）的归一化与回放。
//!
//! TS: `packages/ai/src/utils/transcript.ts`。
//!
//! 一个 transcript 是「首条 system 消息 + 后续消息」：system prompt、sections 与工具声明都折叠进
//! system 消息，之后每次工具变更也是一条 system 消息。本模块做的都是纯回放：
//! [`normalize_context`] 是唯一产出 [`TranscriptContext`] 的入口；
//! [`get_current_system_message`] / [`get_current_tools`] / [`resolve_transcript_tools`] 供各
//! provider 适配层决定「顶层工具字段 vs 就地声明」。
//!
//! TS 用 `readonly { role: string }[]` 接受带自定义 role 的 agent 记录；Rust 侧对应
//! [`TranscriptMessage`]（一个只暴露 `role` 的视图）。
//!
//! # 设计原因与执行流程
//!
//! transcript 是「带增量补丁的消息流」，不是最终快照：会话把 system prompt 的每次变更追加为
//! 一条新的 system 消息（`sections` / `toolsAdded` / `toolsRemoved` 是对前序状态的补丁），
//! 不改写旧消息；每次请求前再由本模块把补丁回放成当前状态。同一份状态因此有多种等价表示，
//! 所有 provider 适配层都必须先经 [`normalize_context`] 归一化（需要时再用
//! [`resolve_transcript`] 按能力折叠）——在 [`TranscriptContext`] 里
//! `Context::system_prompt` 与 `Context::tools` 恒为 `None`，直接读它们会丢内容。
//!
//! 两条主线：
//!
//! 1. 归一化：`Context` → [`normalize_context`] → [`TranscriptContext`]（首条 system 消息
//!    承载 prompt 与初始工具）。
//! 2. 回放：provider 按自己的协议能力选择 [`resolve_transcript`]（是否折叠中途 system
//!    消息）与 [`resolve_transcript_tools`]（顶层工具列表还是就地声明）。
//!
//! 本模块的函数不修改输入；除 [`get_initial_system_message`] 这类返回借用的函数外，其余
//! 返回值是新分配的 [`Tool`] / 消息 / 字符串。
//!
//! # 注意事项
//!
//! - 工具按**名字**识别：同名即同一工具，声明内容变化视为「先移除旧定义、再添加新定义」
//!   （见 [`get_tool_state_changes`]）。
//! - 顺序是协议可见的：每个 system 消息先应用 `toolsRemoved`、再应用 `toolsAdded`；
//!   `IndexMap` 用来保留插入序，删除必须用 `shift_remove` 而不是 `swap_remove`。
//! - JS 普通对象的键序规则（整数索引键升序在前，其余按插入序）由 `js_object_ordered_keys`
//!   复刻，见 [`get_current_system_message`] 与 `utils::text::get_system_message_text`。
//! - 没有 `Result`：缺消息、空工具列表等边界都编码为 `None` / 空集合；至于某个 provider
//!   是否接受中途 system 消息，由调用方通过 `supports_mid_convo_system_messages` 决定。
//! - 相关协议能力开关见 `api::openai_responses_shared::get_responses_tool_support`。

use crate::types::{
    Context, Message, SystemContent, SystemMessage, Tool, ToolReference, TranscriptContext,
};
use crate::utils::text::{content_text_system, get_system_message_text, js_object_ordered_keys};
use indexmap::IndexMap;

/// TS 的 `TranscriptMessages`：只读 `role` 的消息视图。
pub trait TranscriptMessage {
    /// `message.role`。
    fn role(&self) -> &str;
}

impl TranscriptMessage for Message {
    fn role(&self) -> &str {
        Message::role(self)
    }
}

/// 一个具体的 system 消息提供者视图：agent 记录里 role 为 `"system"` 的项。
///
/// TS 的 `isSystemMessage` 只检查 `role === "system"`；Rust 侧判定后需要拿到
/// [`SystemMessage`] 才做回放，因此这个 trait 同时暴露投影。
pub trait SystemMessageView: TranscriptMessage {
    /// 当 `role === "system"` 时返回消息本体。
    fn as_system_message(&self) -> Option<&SystemMessage>;
}

impl SystemMessageView for Message {
    fn as_system_message(&self) -> Option<&SystemMessage> {
        match self {
            Message::System(message) => Some(message),
            _ => None,
        }
    }
}

/// `createInitialSystemMessage(systemPrompt, tools)`。
///
/// 把顶层 `systemPrompt` / `tools` 折叠成一条 system 消息。两者都为空时返回 `None`，
/// 避免在消息列表最前面制造空壳消息；空字符串 prompt 与空工具数组都按「未提供」处理
/// （对应 TS 的 `!== undefined && length > 0`）。
///
/// 生成的消息 `timestamp` 固定为 `0`（`spec/agent-codemode.md` §5.1 的
/// `createInitialSystemMessage` 条目与 `spec/ai-core.md` §2.2.27 都规定恒为 `0`，未说明
/// 理由）；回放时 [`get_current_system_message`] 会保留**第一条** system 消息的时间戳，
/// 后续真实消息的时间戳不会覆盖它。
pub fn create_initial_system_message(
    system_prompt: Option<&str>,
    tools: Option<&[Tool]>,
) -> Option<SystemMessage> {
    let has_system_prompt = system_prompt.is_some_and(|prompt| !prompt.is_empty());
    let has_tools = tools.is_some_and(|tools| !tools.is_empty());
    if !has_system_prompt && !has_tools {
        return None;
    }
    Some(SystemMessage {
        role: Default::default(),
        content: SystemContent::Text(system_prompt.unwrap_or_default().to_string()),
        sections: None,
        tools_added: has_tools.then(|| tools.unwrap_or_default().to_vec()),
        tools_removed: None,
        timestamp: 0,
    })
}

/// `normalizeContext(context)`：把 `systemPrompt` / `tools` 折叠进首条 system 消息。
///
/// 这是唯一产出 [`TranscriptContext`] 的入口；provider 面向的函数都期望它的结果。
///
/// 输入不被修改：返回的消息列表是克隆。初始 system 消息会被插到**最前面**，即使
/// `context.messages` 已经以 system 消息开头（TS 语义如此，可能产生连续两条 system
/// 消息，回放时会把内容合并）。对已归一化的 `Context` 再调用一次是幂等的——此时
/// `system_prompt` 与 `tools` 都是 `None`，不会插入任何东西。
pub fn normalize_context(context: &Context) -> TranscriptContext {
    let initial_message =
        create_initial_system_message(context.system_prompt.as_deref(), context.tools.as_deref());
    let messages = match initial_message {
        Some(initial) => {
            let mut messages = Vec::with_capacity(context.messages.len() + 1);
            messages.push(Message::System(initial));
            messages.extend(context.messages.iter().cloned());
            messages
        }
        None => context.messages.clone(),
    };
    TranscriptContext::from_normalized(Context {
        system_prompt: None,
        messages,
        tools: None,
    })
}

/// `getInitialSystemMessage(messages)`：仅当 transcript 以 system 消息开头时返回它。
///
/// 只看第 0 条；后续出现的 system 消息是增量补丁，不是初始 prompt。返回的是借用。
pub fn get_initial_system_message(messages: &[Message]) -> Option<&SystemMessage> {
    messages.first().and_then(Message::as_system_message)
}

/// `withoutInitialSystemMessage(messages)`：给那些把 prompt 放在消息列表之外的 API。
///
/// 首条是 system 时返回 `messages[1..]` 的克隆，否则整体克隆。TS 在第二种情况直接返回
/// 原数组；Rust 因所有权统一为克隆，调用方不应依赖指针身份。典型用法见
/// `api::google_shared`：先 [`collapse_system_messages`] 再剥掉首条，system 文本单独放进
/// 请求的 `systemInstruction` 字段。
pub fn without_initial_system_message(messages: &[Message]) -> Vec<Message> {
    if get_initial_system_message(messages).is_some() {
        messages[1..].to_vec()
    } else {
        messages.to_vec()
    }
}

/// 抽出所有 system 消息（TS 里各处 `messages.filter(isSystemMessage)`）。
fn system_messages(messages: &[Message]) -> Vec<&SystemMessage> {
    messages
        .iter()
        .filter_map(Message::as_system_message)
        .collect()
}

/// `getCurrentTools(messages)`：按顺序应用每次工具增量后的可用工具。
///
/// 每条 system 消息先应用 `toolsRemoved`、再应用 `toolsAdded`，因此「同一条消息里先删后加
/// 同名工具」的结果是重新加入。返回顺序是首次插入序；被删除后重新添加的工具排到末尾，
/// 这与 TS 的 `Map` 语义一致，也是 provider 看到的工具顺序。
pub fn get_current_tools(messages: &[Message]) -> Vec<Tool> {
    let mut tools: IndexMap<String, Tool> = IndexMap::new();
    for message in system_messages(messages) {
        for tool in message.tools_removed.iter().flatten() {
            // `shift_remove` 保留其余元素的相对顺序；`swap_remove` 会把末尾元素挪到空位，
            // 改变工具顺序（TS 的 `Map.delete` 不重排）。
            tools.shift_remove(&tool.name);
        }
        for tool in message.tools_added.iter().flatten() {
            // `IndexMap::insert` 对已有键只更新值、保留首次位置，对应 `Map.set`。
            tools.insert(tool.name.clone(), tool.clone());
        }
    }
    tools.into_values().collect()
}

/// `getCurrentSystemMessage(messages)`：把每条 system 消息回放成一条首部 system 消息。
///
/// 后续 `content` 追加到基础 prompt（以 `"\n\n"` 连接），`sections` 按名打补丁，工具用
/// [`get_current_tools`] 解析。
///
/// 返回 `None` 当且仅当没有任何 system 消息：Rust 的 [`SystemMessage::timestamp`] 是必需的
/// `i64`，只要有一条 system 消息，`timestamp` 就一定被设置。TS 里该字段可以是
/// `undefined`，所以源码还检查了 `tools.length === 0`；在 Rust 侧这个检查不会改变结果，
/// 保留它是为了与 TS 逐行对应。
///
/// 代价按条目遍历，但不是严格线性：每条 `toolsRemoved` 调用 `IndexMap::shift_remove`
/// （O(当前工具数)，见 [`get_current_tools`]），每个 section 删除调用 `section_order` 的
/// `retain`（O(存活 section 数)）；`js_object_ordered_keys` 还要对索引键做 O(k log k) 排序。
/// 因此重复删除较多时最坏退化为二次复杂度。
pub fn get_current_system_message(messages: &[Message]) -> Option<SystemMessage> {
    let mut content: Vec<String> = Vec::new();
    let mut sections: IndexMap<String, String> = IndexMap::new();
    let mut section_order: Vec<String> = Vec::new();
    let mut timestamp: Option<i64> = None;
    // 阶段 1：按消息顺序回放正文与 section 补丁。`timestamp` 只取第一条 system 消息的值，
    // 后续消息的时间戳不覆盖它；空正文不进入拼接列表（避免多余的分隔符）。
    for message in system_messages(messages) {
        timestamp.get_or_insert(message.timestamp);
        let text = content_text_system(&message.content, "\n");
        if !text.is_empty() {
            content.push(text);
        }
        if let Some(message_sections) = &message.sections {
            // TS: `Object.entries(message.sections ?? {})`——普通对象，整数键在前。
            for name in js_object_ordered_keys(message_sections.keys().map(String::as_str)) {
                let Some(value) = message_sections.get(&name) else {
                    continue;
                };
                match value {
                    None => {
                        sections.shift_remove(&name);
                        section_order.retain(|existing| existing != &name);
                    }
                    Some(value) => {
                        if !sections.contains_key(&name) {
                            section_order.push(name.clone());
                        }
                        sections.insert(name.clone(), value.clone());
                    }
                }
            }
        }
    }
    // `section_order` 记录仍存活的 section 名及其插入顺序：删除时同步移除，之后同名 section
    // 再出现会重新追加到末尾。这正是 TS 里对普通对象 `delete` 再 `set` 的行为。
    // TS 的 `Object.fromEntries(sections)` 产出普通对象：整数键按数值升序排在前面，其余键
    // 保持 Map 的插入序（删除后重新插入回到末尾）。
    let mut ordered_sections: IndexMap<String, Option<String>> = IndexMap::new();
    for name in js_object_ordered_keys(section_order.iter().map(String::as_str)) {
        if let Some(value) = sections.get(&name) {
            ordered_sections.insert(name, Some(value.clone()));
        }
    }
    let tools = get_current_tools(messages);
    if timestamp.is_none() && tools.is_empty() {
        return None;
    }
    Some(SystemMessage {
        role: Default::default(),
        content: SystemContent::Text(content.join("\n\n")),
        sections: (!ordered_sections.is_empty()).then_some(ordered_sections),
        tools_added: (!tools.is_empty()).then_some(tools),
        tools_removed: None,
        timestamp: timestamp.unwrap_or(0),
    })
}

/// `getCurrentSystemPrompt(messages)`。
///
/// 把 [`get_current_system_message`] 的回放结果渲染成一段文本：正文在前，sections 按顺序
/// 以 `"\n\n"` 追加（`None` 的 section 在回放阶段已被删除，不会出现在这里）。没有 system
/// 消息时返回空字符串，调用方无需再判断 `Option`。
pub fn get_current_system_prompt(messages: &[Message]) -> String {
    match get_current_system_message(messages) {
        Some(message) => get_system_message_text(&message),
        None => String::new(),
    }
}

/// `collapseSystemMessages(context)`：为不接受中途 system 消息的 API 重建 transcript。
///
/// 先用 [`get_current_system_message`] 回放出一条头部 system 消息，再过滤掉原列表里的**所有**
/// system 消息，最后把头部放到最前面。代价是丢失 system 消息在对话中的原始位置；只应用于
/// 协议不支持在对话中途插入 system/developer 消息的 API。
///
/// 只要存在 system 消息，头部消息就会保留——即使正文和工具都为空（回放的 timestamp 已设置）。
pub fn collapse_system_messages(context: &TranscriptContext) -> TranscriptContext {
    let messages = &context.as_context().messages;
    let head = get_current_system_message(messages);
    let mut collapsed: Vec<Message> = match head {
        Some(head) => vec![Message::System(head)],
        None => Vec::new(),
    };
    collapsed.extend(
        messages
            .iter()
            .filter(|message| !matches!(message, Message::System(_)))
            .cloned(),
    );
    TranscriptContext::from_normalized(Context {
        system_prompt: None,
        messages: collapsed,
        tools: None,
    })
}

/// `resolveTranscript(context, supportsMidConvoSystemMessages)`。
///
/// `Some(true)` 表示 provider 能在对话中途表达 system 消息：原样返回输入（克隆，不修改）。
/// `None` / `Some(false)` 都走 [`collapse_system_messages`]。该开关来自 provider 兼容配置的
/// `supportsMidConvoSystemMessages`，缺省为 `false`。
pub fn resolve_transcript(
    context: &TranscriptContext,
    supports_mid_convo_system_messages: Option<bool>,
) -> TranscriptContext {
    if supports_mid_convo_system_messages == Some(true) {
        context.clone()
    } else {
        collapse_system_messages(context)
    }
}

/// `toToolDeclaration(tool)`：去掉可执行与仅显示的字段，供比较或持久化。
///
/// TS 的 `JSON.parse(JSON.stringify(tool.parameters))` 会丢弃 `undefined` 字段并归一化键序；
/// Rust 的 [`crate::types::Tool`] 里 `parameters` 已经是 `JsonValue`（没有 `undefined`），
/// 序列化往返等价于一次深复制。实现因此是逐字段克隆，不修改参数 JSON 的内容；克隆本身
/// 不承担归一化职责，比较文本的键序由 `js_object::stringify` 按 JS 规则处理（见
/// [`declarations_equal`]）。
pub fn to_tool_declaration(tool: &Tool) -> Tool {
    Tool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.parameters.clone(),
        constrained_sampling: tool.constrained_sampling.clone(),
    }
}

/// 工具声明的比较文本：`JSON.stringify(toToolDeclaration(tool))`。
fn declaration_text(tool: &Tool) -> String {
    let declaration = to_tool_declaration(tool);
    kiana_json::js_object::stringify(&serde_json::to_value(&declaration).unwrap_or_default())
}

/// `declarationsEqual(left, right)`。
///
/// 比较的是 `JSON.stringify(toToolDeclaration(..))` 文本，而不是结构体相等：这样 `None` 的
/// `constrained_sampling` 因 serde 的 `skip_serializing_if` 被省略，与 TS 展开语义一致。
/// 文本比较对键序敏感，但只对非数组索引键：`js_object::stringify` 会把整数索引键统一按
/// 数值升序重排（与 JS 相同），其余键保持插入序。因此 `parameters` 里普通键的先后不同会
/// 判为不相等，进而被当作一次工具变更（见 [`get_tool_state_changes`]）。这里刻意不做
/// 结构体比较：TS 的 `declarationsEqual` 就是比较 `JSON.stringify` 文本，Rust 侧必须
/// 保持同一判定，否则同一会话在两边会得到不同的工具变更序列。
pub fn declarations_equal(left: &Tool, right: &Tool) -> bool {
    declaration_text(left) == declaration_text(right)
}

/// `ToolStateChanges`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolStateChanges {
    /// 新增或声明内容发生变化的完整工具定义（已做 `toToolDeclaration`）。
    pub tools_added: Vec<Tool>,
    /// 消失或声明内容发生变化的工具名；只含名字，因为移除不需要完整定义。
    ///
    /// 声明内容变化的工具会**同时**出现在 `tools_removed` 与 `tools_added` 中。
    pub tools_removed: Vec<ToolReference>,
}

/// `getToolStateChanges(previous, current)`：定义变化记为「先移除、后添加」。
///
/// 按工具名匹配：改名等价于一删一增。`tools_added` 沿用 `current` 的顺序，`tools_removed`
/// 沿用 `previous` 的顺序；调用方不应假设结果已排序或去重。新增项是深拷贝的声明，
/// 移除项只保留名字。
pub fn get_tool_state_changes(previous: &[Tool], current: &[Tool]) -> ToolStateChanges {
    let previous_tools: IndexMap<&str, &Tool> = previous
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();
    let current_tools: IndexMap<&str, &Tool> = current
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();
    ToolStateChanges {
        tools_added: current
            .iter()
            .filter(|tool| match previous_tools.get(tool.name.as_str()) {
                None => true,
                Some(previous_tool) => !declarations_equal(previous_tool, tool),
            })
            .map(to_tool_declaration)
            .collect(),
        tools_removed: previous
            .iter()
            .filter(|tool| match current_tools.get(tool.name.as_str()) {
                None => true,
                Some(current_tool) => !declarations_equal(tool, current_tool),
            })
            .map(|tool| ToolReference {
                name: tool.name.clone(),
            })
            .collect(),
    }
}

/// `getDeclaredTools(messages)`：transcript 里引用过的所有定义，按首次声明顺序。
///
/// 同名工具再次声明时更新值但保留首次声明的位置（TS `Map.set` 语义），所以顺序是
/// 「首次声明序」而非最新声明序。移除操作不参与：即使工具后来被移除，它仍会出现在结果里
/// ——需要当前可用集合时用 [`get_current_tools`]。
pub fn get_declared_tools(messages: &[Message]) -> Vec<Tool> {
    let mut definitions: IndexMap<String, Tool> = IndexMap::new();
    for message in system_messages(messages) {
        for tool in message.tools_added.iter().flatten() {
            definitions.insert(tool.name.clone(), tool.clone());
        }
    }
    definitions.into_values().collect()
}

/// `hasToolRedefinitions(messages)`。
///
/// 已废弃：内置 transport 不再需要它（Anthropic 用内联 `tool_definition` 块表达重定义）。
/// 保留以维持 API 兼容。
///
/// 判定标准：同一工具名在后续 `toolsAdded` 中出现且声明文本不同。只扫描添加、不扫描移除，
/// 因此「移除后以相同定义重新声明」不算重定义。
#[deprecated(note = "no built-in transport needs this anymore; kept for API compatibility")]
pub fn has_tool_redefinitions(messages: &[Message]) -> bool {
    let mut declared: IndexMap<String, Tool> = IndexMap::new();
    for message in system_messages(messages) {
        for tool in message.tools_added.iter().flatten() {
            if let Some(previous) = declared.get(&tool.name)
                && !declarations_equal(previous, tool)
            {
                return true;
            }
            declared.insert(tool.name.clone(), tool.clone());
        }
    }
    false
}

/// `hasNonAdditiveToolChanges(messages)`。
///
/// 「非增量」指出现了非空的 `toolsRemoved`，或重新声明了一个此前已出现过的名字（即使它
/// 已被删除）。只要存在一次，顶层工具列表就不能继续靠追加维持，需要重新锚定（见
/// [`resolve_transcript_tools`] 的 [`TranscriptToolSupport::Additions`]）。
pub fn has_non_additive_tool_changes(messages: &[Message]) -> bool {
    let systems = system_messages(messages);
    find_last_non_additive_tool_change(&systems) >= 0
}

/// `findLastNonAdditiveToolChange(systemMessages)`：最后一条带移除或同名重声明的 system 消息下标。
///
/// 下标是在**过滤后的 system 消息数组**中的位置，不是原始消息列表的下标；返回 `-1` 表示
/// 从未发生非增量变更。调用方用 `+1` 得到「需要折进顶层工具列表的 system 消息条数」。
fn find_last_non_additive_tool_change(system_messages: &[&SystemMessage]) -> i64 {
    // 之前声明过的名字即视为重声明，即使它后来被移除了：Kimi K3 会忽略被移除后又由后续工具消息
    // 重新声明的工具（2026 年 10 月验证）。
    let mut declared: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut last: i64 = -1;
    for (index, message) in system_messages.iter().enumerate() {
        // 任何非空移除都让此前折进顶层列表的工具状态失效。
        if message
            .tools_removed
            .as_ref()
            .is_some_and(|removed| !removed.is_empty())
        {
            last = index as i64;
        }
        for tool in message.tools_added.iter().flatten() {
            // `declared` 只增不减：判断依据是「这个名字在更早的消息里出现过」，与它当前是否
            // 仍可用无关（见上方 Kimi K3 注释）。
            if declared.contains(&tool.name) {
                last = index as i64;
            }
            declared.insert(tool.name.clone());
        }
    }
    last
}

/// `TranscriptToolSupport`：后续请求里 transport 能表达的工具变更方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TranscriptToolSupport {
    /// 只有顶层工具列表，且总是当前工具。
    #[default]
    None,
    /// 工具可以在后续消息里新增，但不能移除或重定义。
    Additions,
    /// 每个工具（含初始的）都在原处声明，后来的同名声明替换先前的；移除无法在协议里表达。
    Changes,
}

impl TranscriptToolSupport {
    /// TS 的字符串字面量；与 [`TranscriptToolSupport::parse`] 互为逆映射。
    pub fn as_str(self) -> &'static str {
        match self {
            TranscriptToolSupport::None => "none",
            TranscriptToolSupport::Additions => "additions",
            TranscriptToolSupport::Changes => "changes",
        }
    }

    /// 从 TS 字面量解析；未知字符串返回 `None`，不会回退到默认变体。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(TranscriptToolSupport::None),
            "additions" => Some(TranscriptToolSupport::Additions),
            "changes" => Some(TranscriptToolSupport::Changes),
            _ => None,
        }
    }
}

/// `TranscriptTools`。
///
/// 不变量：`folded_system_messages` 是**过滤后的 system 消息数组**的前缀长度，不会超过
/// system 消息总数；这个前缀之后的 system 消息才需要由调用方编码成增量工具项。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TranscriptTools {
    /// 顶层请求字段里的工具。
    pub request_tools: Vec<Tool>,
    /// 开头有多少条 system 消息的工具变更已经被 `request_tools` 持有。
    pub folded_system_messages: usize,
}

/// `resolveTranscriptTools(messages, support)`。
///
/// 决定「顶层请求体带哪些工具」与「开头多少条 system 消息的工具变更已经由顶层列表覆盖」。
/// 调用方应只把 `folded_system_messages` 之后的 system 消息编码成增量工具项，否则同一
/// 变更会被表达两次。
///
/// - [`TranscriptToolSupport::None`]：顶层列表是回放全部消息后的当前工具；所有 system
///   消息都折叠。协议只能靠重新发送完整列表来体现移除与重定义。
/// - [`TranscriptToolSupport::Changes`]：顶层列表为空，所有工具（含初始的）都就地声明；
///   折叠 0 条。
/// - [`TranscriptToolSupport::Additions`]：顶层列表锚定到「最后一次非增量变更」为止的
///   前缀状态，之后只可能追加；没有非增量变更时只折叠列表开头的那条 system 消息（若列表
///   以 system 开头），因此在无移除的会话里这个锚点始终有效，后续新增靠增量项表达。
pub fn resolve_transcript_tools(
    messages: &[Message],
    support: TranscriptToolSupport,
) -> TranscriptTools {
    let systems = system_messages(messages);
    match support {
        // 协议只有顶层列表：发当前完整工具集，system 消息不再单独表达工具变更。
        TranscriptToolSupport::None => TranscriptTools {
            request_tools: get_current_tools(messages),
            folded_system_messages: systems.len(),
        },
        // 每个工具就近声明：顶层为空，没有需要折叠的消息。
        TranscriptToolSupport::Changes => TranscriptTools {
            request_tools: Vec::new(),
            folded_system_messages: 0,
        },
        TranscriptToolSupport::Additions => {
            // 折叠到最后一个非增量变更为止：这段前缀的状态无法用「只增不减」的增量项重建，
            // 只能放进顶层列表；前缀之后只可能出现纯新增。
            let has_initial = get_initial_system_message(messages).is_some();
            let folded = std::cmp::max(
                i64::from(has_initial),
                find_last_non_additive_tool_change(&systems) + 1,
            ) as usize;
            // 只用前缀回放顶层状态，后面的新增留给调用方逐条编码。
            let head: Vec<Message> = systems
                .iter()
                .take(folded)
                .map(|message| Message::System((*message).clone()))
                .collect();
            TranscriptTools {
                request_tools: get_current_tools(&head),
                folded_system_messages: folded,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ModelInputModality, TextContent, TextKind, Tool};
    use serde_json::json;

    fn tool(name: &str, description: &str) -> Tool {
        Tool {
            name: name.into(),
            description: description.into(),
            parameters: json!({"type": "object"}),
            constrained_sampling: None,
        }
    }

    fn system(
        timestamp: i64,
        content: &str,
        added: Option<Vec<Tool>>,
        removed: Option<Vec<&str>>,
    ) -> Message {
        Message::System(SystemMessage {
            role: Default::default(),
            content: SystemContent::Text(content.into()),
            sections: None,
            tools_added: added,
            tools_removed: removed.map(|names| {
                names
                    .into_iter()
                    .map(|name| ToolReference { name: name.into() })
                    .collect()
            }),
            timestamp,
        })
    }

    #[test]
    fn builds_the_initial_system_message_only_when_something_is_set() {
        assert!(create_initial_system_message(None, None).is_none());
        assert!(create_initial_system_message(Some(""), Some(&[])).is_none());
        let message = create_initial_system_message(Some("hi"), None).expect("message");
        assert_eq!(message.content, SystemContent::Text("hi".into()));
        assert_eq!(message.timestamp, 0);
        assert!(message.tools_added.is_none());
        let tools = vec![tool("a", "A")];
        let message = create_initial_system_message(None, Some(&tools)).expect("message");
        assert_eq!(message.tools_added.expect("tools").len(), 1);
    }

    #[test]
    fn normalizes_a_context_into_a_leading_system_message() {
        let context = Context {
            system_prompt: Some("You are helpful.".into()),
            messages: vec![],
            tools: Some(vec![tool("a", "A")]),
        };
        let transcript = normalize_context(&context);
        let messages = &transcript.as_context().messages;
        assert_eq!(messages.len(), 1);
        assert_eq!(get_current_system_prompt(messages), "You are helpful.");
        assert_eq!(get_current_tools(messages).len(), 1);
    }

    #[test]
    fn replays_sections_and_tools() {
        let messages = vec![
            system(5, "base", Some(vec![tool("a", "A"), tool("b", "B")]), None),
            Message::System(SystemMessage {
                role: Default::default(),
                content: SystemContent::Text("extra".into()),
                sections: Some(
                    [("one".to_string(), Some("1".to_string()))]
                        .into_iter()
                        .collect(),
                ),
                tools_added: Some(vec![tool("c", "C")]),
                tools_removed: Some(vec![ToolReference { name: "a".into() }]),
                timestamp: 9,
            }),
        ];
        let current = get_current_system_message(&messages).expect("message");
        assert_eq!(current.content, SystemContent::Text("base\n\nextra".into()));
        assert_eq!(current.timestamp, 5);
        let names: Vec<String> = current
            .tools_added
            .expect("tools")
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert_eq!(names, vec!["b", "c"]);
        // `getSystemMessageText` 把 sections 追加在正文之后。
        assert_eq!(get_current_system_prompt(&messages), "base\n\nextra\n\n1");
    }

    #[test]
    fn null_sections_remove_and_reappear_at_the_end() {
        let mut first_sections = IndexMap::new();
        first_sections.insert("keep".to_string(), Some("1".to_string()));
        first_sections.insert("gone".to_string(), Some("2".to_string()));
        let mut second_sections = IndexMap::new();
        second_sections.insert("gone".to_string(), None);
        let messages = vec![
            Message::System(SystemMessage {
                role: Default::default(),
                content: SystemContent::Text("base".into()),
                sections: Some(first_sections),
                tools_added: None,
                tools_removed: None,
                timestamp: 1,
            }),
            Message::System(SystemMessage {
                role: Default::default(),
                content: SystemContent::Text(String::new()),
                sections: Some(second_sections),
                tools_added: None,
                tools_removed: None,
                timestamp: 2,
            }),
        ];
        let current = get_current_system_message(&messages).expect("message");
        let sections = current.sections.expect("sections");
        assert_eq!(sections.len(), 1);
        assert_eq!(sections.get("keep"), Some(&Some("1".to_string())));
    }

    #[test]
    fn collapses_mid_conversation_system_messages() {
        let messages = vec![
            system(1, "base", None, None),
            system(2, "extra", Some(vec![tool("a", "A")]), None),
        ];
        let transcript = TranscriptContext::from_normalized(Context {
            system_prompt: None,
            messages,
            tools: None,
        });
        let collapsed = collapse_system_messages(&transcript);
        let messages = &collapsed.as_context().messages;
        assert_eq!(messages.len(), 1);
        assert_eq!(get_current_system_prompt(messages), "base\n\nextra");
        let same = resolve_transcript(&transcript, Some(true));
        assert_eq!(same, transcript);
    }

    #[test]
    fn compares_tool_declarations() {
        let left = tool("a", "A");
        let right = tool("a", "A");
        assert!(declarations_equal(&left, &right));
        let changed = tool("a", "B");
        assert!(!declarations_equal(&left, &changed));
        let changes = get_tool_state_changes(
            std::slice::from_ref(&left),
            &[changed.clone(), tool("b", "B")],
        );
        assert_eq!(changes.tools_added.len(), 2);
        assert_eq!(changes.tools_removed.len(), 1);
        assert_eq!(changes.tools_removed[0].name, "a");
    }

    #[test]
    fn resolves_transcript_tools_per_support() {
        let messages = vec![
            system(1, "base", Some(vec![tool("a", "A")]), None),
            system(2, "", None, Some(vec!["a"])),
            system(3, "", Some(vec![tool("b", "B")]), None),
        ];
        let none = resolve_transcript_tools(&messages, TranscriptToolSupport::None);
        assert_eq!(none.request_tools.len(), 1);
        assert_eq!(none.request_tools[0].name, "b");
        assert_eq!(none.folded_system_messages, 3);

        let changes = resolve_transcript_tools(&messages, TranscriptToolSupport::Changes);
        assert!(changes.request_tools.is_empty());
        assert_eq!(changes.folded_system_messages, 0);

        // 最后一次非增量变更在 system message #2（移除 a），因此折叠到它为止：顶层工具是
        // 那之后的工具状态（空），后续新增保持锚定。
        let additions = resolve_transcript_tools(&messages, TranscriptToolSupport::Additions);
        assert!(additions.request_tools.is_empty());
        assert_eq!(additions.folded_system_messages, 2);
        assert!(has_non_additive_tool_changes(&messages));

        let additive = vec![
            system(1, "base", Some(vec![tool("a", "A")]), None),
            system(2, "", Some(vec![tool("b", "B")]), None),
        ];
        assert!(!has_non_additive_tool_changes(&additive));
        let additions = resolve_transcript_tools(&additive, TranscriptToolSupport::Additions);
        assert_eq!(additions.folded_system_messages, 1);
    }

    #[test]
    fn declared_tools_keep_the_first_declaration_order() {
        let messages = vec![
            system(1, "", Some(vec![tool("a", "A"), tool("b", "B")]), None),
            system(2, "", Some(vec![tool("a", "A2")]), None),
        ];
        let declared = get_declared_tools(&messages);
        let names: Vec<&str> = declared.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        // `Map.set` 对已有键保位但更新值。
        assert_eq!(declared[0].description, "A2");
        #[allow(deprecated)]
        {
            assert!(has_tool_redefinitions(&messages));
        }
    }

    #[test]
    fn tools_without_initial_system_message_still_resolve() {
        let messages = vec![Message::User(crate::types::UserMessage {
            role: Default::default(),
            content: crate::types::UserContent::Blocks(vec![crate::types::UserContentBlock::Text(
                TextContent {
                    kind_text: TextKind::default(),
                    text: "hi".into(),
                    text_signature: None,
                },
            )]),
            timestamp: 1,
        })];
        let additions = resolve_transcript_tools(&messages, TranscriptToolSupport::Additions);
        assert_eq!(additions.folded_system_messages, 0);
        assert!(additions.request_tools.is_empty());
        let _ = ModelInputModality::Text;
    }
    #[test]
    fn replayed_sections_follow_js_object_key_order() {
        let mut sections = IndexMap::new();
        sections.insert("10".to_string(), Some("ten".to_string()));
        sections.insert("2".to_string(), Some("two".to_string()));
        sections.insert("b".to_string(), Some("bee".to_string()));
        sections.insert("a".to_string(), Some("ay".to_string()));
        let messages = vec![Message::System(SystemMessage {
            role: Default::default(),
            content: SystemContent::Text("base".into()),
            sections: Some(sections),
            tools_added: None,
            tools_removed: None,
            timestamp: 1,
        })];
        let current = get_current_system_message(&messages).expect("message");
        let names: Vec<&str> = current
            .sections
            .as_ref()
            .expect("sections")
            .keys()
            .map(String::as_str)
            .collect();
        // TS 的 `Object.fromEntries` 产出普通对象：整数键升序在前。
        assert_eq!(names, vec!["2", "10", "b", "a"]);
        assert_eq!(
            get_current_system_prompt(&messages),
            "base\n\ntwo\n\nten\n\nbee\n\nay"
        );
    }
}
