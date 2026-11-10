//! `harness/prompt.ts`：系统提示分段的重放、渲染与 `kiana.system` 条目的规划。
//!
//! TS: `packages/durable/src/harness/prompt.ts`。
//!
//! # 这个模块解决什么问题
//!
//! 系统提示不是每次请求重写一条完整记录，而是以 `kiana.system` 条目增量叠加：每条消息的
//! `sections` 是「设置 / 删除命名分段」的补丁，`toolsAdded` / `toolsRemoved` 是工具声明
//! 补丁。本模块负责三件事：
//!
//! 1. [`replay_sections`]：把已提交的消息回放成当前生效的分段；
//! 2. [`render_sections`]：按 agent 的分段顺序重新渲染出目标分段；
//! 3. [`plan_system_entries`]：把目标与现状求差，规划出需要追加的 `kiana.system` 条目。
//!
//! 没有它，每次请求要么重复写入整段提示，要么无法在 reset / 压缩留下的新 head 标记之后
//! 重建完整的分段与工具。
//!
//! # 三个步骤的衔接
//!
//! `crate::harness::generation` 的 `prepare` 是生产调用点：
//!
//! 1. `shown = replay_sections(&view.messages)`：当前转录里已生效的分段文本；
//! 2. `desired = render_sections(&agent.sections, &input, &shown, &mut report, context)`：
//!    逐个渲染，失败的分段保留 `shown` 里的旧文本并上报；
//! 3. `drafts = plan_system_entries(&view, &desired, &tools, now)`：调用方把返回的草稿按
//!    顺序追加为 `kiana.system` 条目。
//!
//! `examples/14_system_prompt.rs` 与 `tests/harness_prompt.rs` 也直接调用这三个函数。
//!
//! # 顺序稳定性（本模块的主要复杂度）
//!
//! TS 里 `sections` 是普通对象，`Object.entries` / `Object.fromEntries` 的枚举序是「整数索引
//! 键升序在前，其余键保持插入序」；回放用的 `Map` 则是纯插入序，删除后重新加入会排到末尾。
//! `kiana.system` 条目会逐字节持久化，测试用 TS 实测的黄金值锁定这些顺序，所以本模块在
//! 进出 JS 对象语义的边界上统一走 `js_ordered_sections`，并用 `IndexMap` 模拟 `Map`。
//!
//! # 特殊键 `__proto__`
//!
//! 最小补丁在 TS 里由普通对象的属性赋值构造，`patch["__proto__"] = …` 命中的是
//! `Object.prototype` 上的访问器而不是自有属性，因此最小补丁永远改不动名为 `__proto__` 的
//! 分段；而 `Object.fromEntries`（基线、顺序重写）用 CreateDataProperty，会创建自有键。
//! 这看起来像缺陷，但它是被测试锁定的 TS 行为，必须保留（见 `plan_sections` 与回归测试）。
//!
//! # 边界与失败模式
//!
//! - 三个入口都不写存储、不做网络 IO；渲染时 `PromptSection` 的实现可能读文件或文档（输入
//!   由 `PromptInput` 提供）。
//! - [`render_sections`] 是唯一的 `Result` 入口：只有上下文已取消时才把分段错误向上传播，
//!   其余错误交给 `report` 并继续渲染后面的分段。
//! - [`plan_system_entries`] 可能返回空 `Vec`（没有任何变化），调用方据此不追加条目。

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;
use kiana_ai::utils::transcript::{declarations_equal, get_current_tools, to_tool_declaration};
use kiana_ai::{Message, SystemContent, SystemMessage, SystemRole, Tool, ToolReference};
use kiana_chord::context::Context;

use crate::harness::types::{ContextView, PromptInput, PromptSection};
use crate::types::{
    BoxError, ContextEdit, ContextEditAction, NoData, SystemEntry, TypedEntryDraft,
};

/// 一次计划出的 `kiana.system` 条目。
///
/// `data` 用 [`NoData`]：TS 的 `TypedEntryDraft<never>` 没有 `data` 键，追加条目时不会写
/// `data`。`model` 恰好一条 system 消息；`head` 恒为 `None`，只有基线分支会填 `edits`。
pub type SystemDraft = TypedEntryDraft<NoData>;

/// JS 的「数组索引」键：`ToString(ToUint32(k)) === k && k !== "4294967295"`。
///
/// 与 `kiana_json::js_object` 的私有 `is_array_index` 同一规则；`kiana-json` 没有导出它。
///
/// 这里判断的是「JS 对象会不会把它当作数组下标来枚举」，与是否真的读取数组元素无关：
/// `"0"`、`"10"` 是索引；`"01"`、`"-1"`、`"1.0"`、`"4294967295"`（`u32::MAX`，不是合法
/// 索引）都不是。
fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 {
        return None;
    }
    if key.len() > 1 && key.starts_with('0') {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value: u32 = key.parse().ok()?;
    (value != u32::MAX).then_some(value)
}

/// 分段映射按 JS 对象的键序重排：整数索引键升序在前，其余保持插入序。
///
/// TS 的 `sections` 是普通对象（`Record<string, string | null>`），`Object.entries`、
/// `Object.fromEntries` 与属性赋值产生的对象都以该顺序枚举，与 `Map` 的纯插入序不同。
/// `kiana.system` 的持久化值与 `replaySections` 的回放顺序都依赖它。
///
/// 只调整枚举顺序，不改变键值本身；对只含非索引键的映射是保序的。基线、顺序重写与最小补丁
/// 在交给 [`system_entry`] 之前都经过它，因此持久化出去的 `sections` 总是 JS 对象键序。
fn js_ordered_sections(
    sections: &IndexMap<String, Option<String>>,
) -> IndexMap<String, Option<String>> {
    // 先按 JS 规则分两组：数组索引键按数值升序排最前，其余键保持插入序。
    let mut indices: Vec<(u32, &String, &Option<String>)> = Vec::new();
    let mut others: Vec<(&String, &Option<String>)> = Vec::new();
    for (key, value) in sections {
        match array_index(key) {
            Some(index) => indices.push((index, key, value)),
            None => others.push((key, value)),
        }
    }
    indices.sort_by_key(|(index, _, _)| *index);
    let mut ordered = IndexMap::with_capacity(sections.len());
    for (_, key, value) in indices {
        ordered.insert(key.clone(), value.clone());
    }
    for (key, value) in others {
        ordered.insert(key.clone(), value.clone());
    }
    ordered
}

/// 按顺序重放 system 消息之后生效的分段：就地设置，`null` 删除，重新加入则追加。
///
/// TS: `replaySections`。每条消息的 `sections` 先按 JS 对象键序枚举（见
/// `js_ordered_sections`），因为 TS 用 `Object.entries(message.sections)`；结果 `Map` 的
/// 位置规则是「已存在的键 `set` 保持原位，删除后重加追加到末尾」。
///
/// 参数 `messages` 是视图的模型上下文（从旧到新）。只处理 [`Message::System`] 且
/// `sections` 为 `Some` 的消息；其它角色、没有分段的 system 消息（纯工具增量条目）都被跳过。
///
/// 返回当前已显示分段的值与顺序。函数不返回错误，也不修改输入。
pub fn replay_sections(messages: &[Message]) -> IndexMap<String, String> {
    let mut shown: IndexMap<String, String> = IndexMap::new();
    for message in messages {
        let Message::System(system) = message else {
            continue;
        };
        let Some(sections) = &system.sections else {
            continue;
        };
        for (key, value) in js_ordered_sections(sections) {
            match value {
                None => {
                    shown.shift_remove(&key);
                }
                Some(value) => {
                    shown.insert(key, value);
                }
            }
        }
    }
    shown
}

/// 按顺序渲染 agent 的分段。`None` 省略该分段；带标签的文本包成 `<key>\n...\n</key>`。抛错的分段
/// 保留它已显示的文本（如果有）并被报告；`context` 已取消之后的错误向上抛。
///
/// # 参数
///
/// - `sections`：agent 的分段列表，顺序就是渲染与返回顺序（`resolve_agent` 合并后的结果）；
/// - `input`：传给每个分段渲染器的上下文（agent、env、已提交文档读取等）；
/// - `shown`：上一版已生效的分段文本，仅在某个分段渲染失败时用来保留旧文本；
/// - `report`：渲染失败（且上下文未取消）时逐次调用，参数是错误与该分段；生产调用方把它
///   转发成按扩展归因的上报；
/// - `context`：传给 `PromptSection::render`，并用于判断取消。
///
/// # 返回与错误
///
/// 返回按分段顺序排列的目标文本。渲染返回 `Ok(None)` 的分段被省略（不进入结果，后续规划会
/// 删除它）；`tag() == Some(false)` 的文本原样放入，其余包成 `<key>\n{text}\n</key>`（当前
/// 稳定契约，改动需同步测试）。同名 key 在 `sections` 里出现多次时，结果的位置取第一次、值取最后一次。
///
/// 分段是**串行** `await` 的，不是并发渲染。唯一的 `Err` 路径是某分段返回错误、且
/// `context.abort_signal()` 存在并已取消：此时原样返回该错误，不再调用 `report`，也不再渲染
/// 后续分段。否则错误被报告；若 `shown` 里有同 key 的旧文本，就把它原样保留在结果里，继续
/// 处理下一个分段。
///
/// TS: `renderSections`
pub async fn render_sections(
    sections: &[Arc<dyn PromptSection>],
    input: &PromptInput,
    shown: &IndexMap<String, String>,
    report: &mut (dyn FnMut(BoxError, &Arc<dyn PromptSection>) + Send),
    context: &dyn Context,
) -> Result<IndexMap<String, String>, BoxError> {
    let mut desired: IndexMap<String, String> = IndexMap::new();
    for section in sections {
        let text = match section.render(input, context).await {
            Ok(text) => text,
            Err(error) => {
                if context
                    .abort_signal()
                    .is_some_and(|signal| signal.is_aborted())
                {
                    return Err(error);
                }
                report(error, section);
                if let Some(kept) = shown.get(section.key()) {
                    desired.insert(section.key().to_owned(), kept.clone());
                }
                continue;
            }
        };
        let Some(text) = text else {
            continue;
        };
        let text = if section.tag() == Some(false) {
            text
        } else {
            format!("<{}>\n{}\n</{}>", section.key(), text, section.key())
        };
        desired.insert(section.key().to_owned(), text);
    }
    Ok(desired)
}

/// 规划让 `view` 的重放分段与工具在值和顺序上都等于 `desired` 与 `tools` 的 `kiana.system` 条目。
///
/// - 有 head 标记且上下文里没有更晚的 `kiana.system` 条目：一个完整基线，省略每个被保留的更早
///   `kiana.system` 条目；即使它重述了重放值也要写。
/// - 否则，当最小分段补丁会留下不同顺序时：移除每个已显示分段，再按顺序重新加入每个目标分段。
/// - 否则就是改动值与 `null` 删除的最小补丁，或者什么都不写。
///
/// 工具变化搭在最后一条计划条目上，或单独形成一条。
///
/// # 参数
///
/// - `view`：上下文视图。`view.head` 是最新适用的 head 标记；`view.entries` 是活动条目
///   （含标记自身）；`view.messages` 是当前模型上下文，分段与工具都从它回放。函数内部
///   重新回放，不接收调用方算好的 `shown`；调用方应保证渲染用的视图与这里是同一个，否则
///   补丁会以另一份状态为基准。
/// - `desired`：`render_sections` 的结果，顺序即期望的分段顺序。
/// - `tools`：agent 提供的工具，顺序即期望的工具顺序；名字应唯一。
/// - `timestamp`：写进每条草稿 system 消息的时间戳（epoch 毫秒）。
///
/// # 两个分支
///
/// 1. **基线**：`view.head` 存在，且 `view.entries` 里没有 `id > head.id` 的
///    `kiana.system` 条目（标记自身不算）。这说明 head 标记之后还没写过基线；返回一条完整
///    条目：全部 `desired` 分段 + 全部工具声明，并对 `view.entries` 里每个 `kiana.system`
///    条目加 `omit` 覆盖（填进 `edits`），让它们不再贡献上下文。即使基线与回放值相同也
///    照写——它就是新基线的第一条。
/// 2. **增量**：否则返回 0..2 条。分段补丁由 `plan_sections` 计算，工具变化由 `plan_tools`
///    计算；没有工具变化时每条补丁单独成条；有工具变化时搭在最后一条补丁上，没有分段补丁
///    就单独成条。`toolsRemoved` / `toolsAdded` 各自为空时对应字段省略。
///
/// 返回空 `Vec` 表示没有任何变化。函数是纯函数：不读存储、不写条目，也没有错误。
///
/// # 不变量
///
/// 草稿按顺序追加后，回放出的分段在 JS 对象键序下等于 `desired`、工具等于 `tools`（见
/// `tests/harness_prompt.rs` 的 `apply` / `apply_tools`）。唯一的例外是 `__proto__` 分段：
/// 最小补丁按 TS 语义改不动它，见 `plan_sections`。
///
/// TS: `planSystemEntries`
pub fn plan_system_entries(
    view: &ContextView,
    desired: &IndexMap<String, String>,
    tools: &[Tool],
    timestamp: i64,
) -> Vec<SystemDraft> {
    if let Some(head) = &view.head
        && !view
            .entries
            .iter()
            .any(|entry| SystemEntry.is(entry) && entry.id > head.id)
    {
        let edits: Vec<ContextEdit> = view
            .entries
            .iter()
            .filter(|entry| SystemEntry.is(entry))
            .map(|entry| ContextEdit {
                target: entry.id,
                action: ContextEditAction::Omit,
                messages: None,
            })
            .collect();
        let baseline_sections: IndexMap<String, Option<String>> = desired
            .iter()
            .map(|(key, value)| (key.clone(), Some(value.clone())))
            .collect();
        let baseline = system_entry(
            Some(js_ordered_sections(&baseline_sections)),
            Some(&ToolChanges {
                tools_removed: Vec::new(),
                tools_added: tools.iter().map(to_tool_declaration).collect(),
            }),
            timestamp,
        );
        if edits.is_empty() {
            return vec![baseline];
        }
        let mut baseline = baseline;
        baseline.edits = Some(edits);
        return vec![baseline];
    }
    let sections = plan_sections(&replay_sections(&view.messages), desired);
    let changes = plan_tools(&get_current_tools(&view.messages), tools);
    if changes.tools_removed.is_empty() && changes.tools_added.is_empty() {
        return sections
            .into_iter()
            .map(|patch| system_entry(Some(patch), None, timestamp))
            .collect();
    }
    if sections.is_empty() {
        return vec![system_entry(None, Some(&changes), timestamp)];
    }
    let last = sections.len() - 1;
    sections
        .into_iter()
        .enumerate()
        .map(|(index, patch)| {
            system_entry(
                Some(patch),
                if index == last { Some(&changes) } else { None },
                timestamp,
            )
        })
        .collect()
}

/// 从 `offered` 到 `desired` 的工具变化。声明改变的工具被移除并重新加入。重放会把保留的工具留在
/// 原位并追加新增项；若这样得不到目标顺序，就移除每个提供的工具，再按顺序重新加入每个目标工具。
///
/// `offered` 是回放当前消息得到的工具（`get_current_tools`），其顺序是当前提供顺序；
/// `desired` 是 agent 的工具，其顺序是期望顺序。名字集合应唯一：当前调用链由
/// `resolve_agent` 与 `get_current_tools` 保证按名字去重。`plan_tools` 自身不校验唯一性，
/// 而下面的逐位比较直接索引 `desired[index]`、没有越界检查：重名输入（例如重复的
/// `offered` 项也被保留进 `kept`）可能让 `replayed` 比 `desired` 长并在该比较中越界 panic，
/// 因此唯一性由调用方负责。`declarations_equal` 比较的是声明 JSON 文本，因此参数键序变化
/// 也算一次声明变化（同一工具会在 `tools_removed` 与 `tools_added` 里各出现一次）。
///
/// 返回的两个列表都保持输入顺序：`tools_removed` 沿用 `offered` 的顺序，`tools_added` 沿用
/// `desired` 的顺序。顺序不一致时走「全部移除 + 全部按目标顺序重加」。
fn plan_tools(offered: &[Tool], desired: &[Tool]) -> ToolChanges {
    // 名字 → 期望声明（同名时后者覆盖前者；调用方保证名字唯一）。
    let wanted: IndexMap<&str, &Tool> = desired
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();
    // 保留 = 名字在目标里且声明文本逐字段相同；声明变化的工具按「移除再添加」处理。
    let kept: Vec<Tool> = offered
        .iter()
        .filter(|tool| {
            wanted
                .get(tool.name.as_str())
                .is_some_and(|next| declarations_equal(tool, next))
        })
        .cloned()
        .collect();
    let kept_names: HashSet<&str> = kept.iter().map(|tool| tool.name.as_str()).collect();
    let added: Vec<Tool> = desired
        .iter()
        .filter(|tool| !kept_names.contains(tool.name.as_str()))
        .cloned()
        .collect();
    // 重放顺序 = 保留（原序）+ 新增（目标序）；与目标顺序不一致就全部移除、按目标顺序重加。
    let replayed: Vec<&Tool> = kept.iter().chain(added.iter()).collect();
    if replayed
        .iter()
        .enumerate()
        .any(|(index, tool)| tool.name != desired[index].name)
    {
        return ToolChanges {
            tools_removed: offered
                .iter()
                .map(|tool| ToolReference {
                    name: tool.name.clone(),
                })
                .collect(),
            tools_added: desired.iter().map(to_tool_declaration).collect(),
        };
    }
    ToolChanges {
        tools_removed: offered
            .iter()
            .filter(|tool| !kept_names.contains(tool.name.as_str()))
            .map(|tool| ToolReference {
                name: tool.name.clone(),
            })
            .collect(),
        tools_added: added.iter().map(to_tool_declaration).collect(),
    }
}

/// 唯一一个会被「普通对象属性赋值」特殊对待的键：`Object.prototype` 上的 `__proto__` 访问器。
const JS_PROTOTYPE_KEY: &str = "__proto__";

/// 分段补丁：没有、最小补丁，或顺序会不同时的「全部移除再全部重加」一对。
///
/// TS 的补丁是普通对象（`Object.fromEntries` / 属性赋值），枚举序永远是 JS 对象键序，所以
/// 每个补丁在交给 [`system_entry`] 之前都用 [`js_ordered_sections`] 归一。
///
/// `Object.fromEntries` 用 `CreateDataProperty`，会创建自有的 `__proto__` 键（重写分支与基线
/// 都保留它）；而最小补丁走的是属性赋值，命中的是原型访问器，**不会**创建自有属性，所以
/// 名为 `__proto__` 的分段在最小补丁里既不能被加入、修改，也不能被删除。
///
/// 算法分三步：
/// 1. 预测最小补丁会得到的分段顺序：保留下来的已显示键保持原序，目标里新增的键按目标顺序
///    追加；把它与目标顺序逐位比较。
/// 2. 顺序不一致就返回两个补丁：先把 `shown` 的每个键置 `null`，再按 `desired` 顺序全部设值。
/// 3. 顺序一致则构造最小补丁：`shown` 里值变化或目标缺失的键写新值 / `null`，再补上目标里
///    新增的键；补丁为空表示这次不需要写条目。
///
/// 返回的每个补丁都按 JS 对象键序排好；`None` 表示删除该分段。
fn plan_sections(
    shown: &IndexMap<String, String>,
    desired: &IndexMap<String, String>,
) -> Vec<IndexMap<String, Option<String>>> {
    // 1. 预测最小补丁后的顺序：保留的键按当前顺序，新增的键按目标顺序追加。
    let patched_order: Vec<&String> = shown
        .keys()
        .filter(|key| desired.contains_key(*key))
        .chain(desired.keys().filter(|key| !shown.contains_key(*key)))
        .collect();
    let desired_order: Vec<&String> = desired.keys().collect();
    if patched_order
        .iter()
        .zip(&desired_order)
        .any(|(key, expected)| key != expected)
    {
        // 2. 顺序对不上：先删除每个已显示分段，再按目标顺序全部重新加入。
        return vec![
            js_ordered_sections(
                &shown
                    .keys()
                    .map(|key| (key.clone(), None))
                    .collect::<IndexMap<_, _>>(),
            ),
            js_ordered_sections(
                &desired
                    .iter()
                    .map(|(key, value)| (key.clone(), Some(value.clone())))
                    .collect(),
            ),
        ];
    }

    // 3. 顺序一致：只写值变化、删除与新增的键；`None` 表示删除。
    let mut patch: IndexMap<String, Option<String>> = IndexMap::new();
    for (key, value) in shown {
        // TS 的补丁是普通对象（`const patch: Record<…> = {}`）：`patch["__proto__"] = …`
        // 命中 `Object.prototype` 的访问器，字符串值是空操作、`null` 只把原型设成 null，
        // 都不会创建自有属性。因此最小补丁永远改不动名为 `__proto__` 的分段
        // （`Object.keys(patch).length` 也不把它算进去）。
        if key == JS_PROTOTYPE_KEY {
            continue;
        }
        match desired.get(key) {
            Some(next) if next == value => {}
            other => {
                patch.insert(key.clone(), other.cloned());
            }
        }
    }
    for (key, value) in desired {
        if !shown.contains_key(key) && key != JS_PROTOTYPE_KEY {
            patch.insert(key.clone(), Some(value.clone()));
        }
    }
    if patch.is_empty() {
        Vec::new()
    } else {
        vec![js_ordered_sections(&patch)]
    }
}

/// 组装一条 `kiana.system` 草稿（TS: `systemEntry`）。
///
/// `content` 恒为空串，`model` 只有一条 system 消息；`head` / `edits` 留空，基线分支之后
/// 再填 `edits`。`sections` 为 `Some` 时原样写入（空表也写 `sections: {}`，基线在目标为空
/// 时就是这样）；`tools_added` / `tools_removed` 为空向量时转成 `None`，序列化时省略字段
/// （对应 TS 的空数组不展开）。
///
/// 持久化键序由 `crate::types::persisted_message_json` 统一重排（`toolsRemoved` 在
/// `toolsAdded` 之前），不依赖这里构造的那条消息的字段序。
fn system_entry(
    sections: Option<IndexMap<String, Option<String>>>,
    tools: Option<&ToolChanges>,
    timestamp: i64,
) -> SystemDraft {
    let message = SystemMessage {
        role: SystemRole::default(),
        content: SystemContent::Text(String::new()),
        sections,
        tools_added: tools
            .filter(|tools| !tools.tools_added.is_empty())
            .map(|tools| tools.tools_added.clone()),
        tools_removed: tools
            .filter(|tools| !tools.tools_removed.is_empty())
            .map(|tools| tools.tools_removed.clone()),
        timestamp,
    };
    TypedEntryDraft {
        model: Some(vec![Message::System(message)]),
        data: NoData,
        head: None,
        edits: None,
    }
}

/// 工具变化的两个列表（TS: `ToolChanges`）。
struct ToolChanges {
    /// 要移除的工具名，沿用当前提供顺序；声明变化的工具也会在这里出现一次。
    tools_removed: Vec<ToolReference>,
    /// 要（重新）加入的完整声明，沿用目标顺序。
    tools_added: Vec<Tool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EntryRecord;

    fn system(
        sections: Option<IndexMap<String, Option<String>>>,
        tools_added: Vec<Tool>,
    ) -> Message {
        Message::System(SystemMessage {
            role: SystemRole::default(),
            content: SystemContent::Text(String::new()),
            sections,
            tools_added: if tools_added.is_empty() {
                None
            } else {
                Some(tools_added)
            },
            tools_removed: None,
            timestamp: 0,
        })
    }

    fn section_map(entries: &[(&str, Option<&str>)]) -> IndexMap<String, Option<String>> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.map(str::to_owned)))
            .collect()
    }

    fn tool(name: &str, description: &str) -> Tool {
        Tool {
            name: name.to_owned(),
            description: description.to_owned(),
            parameters: serde_json::json!({"type": "object"}),
            constrained_sampling: None,
        }
    }

    fn system_entry_record(id: i64) -> EntryRecord {
        EntryRecord {
            key_order: None,
            model: None,
            data: None,
            edits: None,
            kind: "kiana.system".to_owned(),
            id: crate::types::id_from_number(id),
            conversation_id: crate::types::id_from_number(1),
            head: None,
            by_task_id: None,
        }
    }

    #[test]
    fn replay_sets_deletes_and_reappends() {
        let messages = vec![
            system(
                Some(section_map(&[("a", Some("1")), ("b", Some("2"))])),
                Vec::new(),
            ),
            system(
                Some(section_map(&[
                    ("a", None),
                    ("b", Some("3")),
                    ("c", Some("4")),
                ])),
                Vec::new(),
            ),
        ];
        let shown = replay_sections(&messages);
        assert_eq!(
            shown.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["b", "c"]
        );
        assert_eq!(shown["b"], "3");
    }

    #[test]
    fn a_deleted_then_readded_section_appends() {
        let messages = vec![
            system(
                Some(section_map(&[("a", Some("1")), ("b", Some("2"))])),
                Vec::new(),
            ),
            system(Some(section_map(&[("a", None)])), Vec::new()),
            system(Some(section_map(&[("a", Some("3"))])), Vec::new()),
        ];
        let shown = replay_sections(&messages);
        assert_eq!(
            shown.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["b", "a"]
        );
    }

    #[test]
    fn the_first_plan_is_a_baseline_that_omits_retained_system_entries() {
        let view = ContextView {
            head: Some(EntryRecord {
                head: Some(crate::types::id_from_number(1)),
                ..system_entry_record(5)
            }),
            entries: vec![system_entry_record(2), system_entry_record(3)],
            contributions: Vec::new(),
            messages: Vec::new(),
        };
        let mut desired = IndexMap::new();
        desired.insert("alpha".to_owned(), "one".to_owned());
        let planned = plan_system_entries(&view, &desired, &[tool("read", "Read")], 9);
        assert_eq!(planned.len(), 1);
        let draft = &planned[0];
        let Message::System(message) = &draft.model.as_ref().unwrap()[0] else {
            panic!("expected a system message");
        };
        assert_eq!(
            message.sections.as_ref().unwrap()["alpha"],
            Some("one".to_owned())
        );
        assert_eq!(message.tools_removed, None);
        assert_eq!(message.tools_added.as_ref().unwrap()[0].name, "read");
        assert_eq!(message.timestamp, 9);
        let edits = draft.edits.as_ref().unwrap();
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].action, ContextEditAction::Omit);
    }

    #[test]
    fn without_a_head_a_patch_only_lists_changes() {
        let messages = vec![system(
            Some(section_map(&[
                ("alpha", Some("one")),
                ("beta", Some("two")),
            ])),
            Vec::new(),
        )];
        let view = ContextView {
            head: None,
            entries: vec![system_entry_record(1)],
            contributions: Vec::new(),
            messages,
        };
        let mut desired = IndexMap::new();
        desired.insert("alpha".to_owned(), "one".to_owned());
        desired.insert("beta".to_owned(), "TWO".to_owned());
        let planned = plan_system_entries(&view, &desired, &[], 1);
        assert_eq!(planned.len(), 1);
        let Message::System(message) = &planned[0].model.as_ref().unwrap()[0] else {
            panic!("expected a system message");
        };
        let sections = message.sections.as_ref().unwrap();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections["beta"], Some("TWO".to_owned()));
        assert!(message.tools_added.is_none());
        assert!(message.tools_removed.is_none());
    }

    #[test]
    fn a_different_order_rewrites_every_section() {
        let messages = vec![system(
            Some(section_map(&[
                ("alpha", Some("one")),
                ("beta", Some("two")),
            ])),
            Vec::new(),
        )];
        let view = ContextView {
            head: None,
            entries: vec![system_entry_record(1)],
            contributions: Vec::new(),
            messages,
        };
        let mut desired = IndexMap::new();
        desired.insert("beta".to_owned(), "two".to_owned());
        desired.insert("alpha".to_owned(), "one".to_owned());
        let planned = plan_system_entries(&view, &desired, &[], 1);
        assert_eq!(planned.len(), 2);
        let Message::System(first) = &planned[0].model.as_ref().unwrap()[0] else {
            panic!("expected a system message");
        };
        let first_sections = first.sections.as_ref().unwrap();
        assert_eq!(
            first_sections
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
        assert!(first_sections.values().all(Option::is_none));
        let Message::System(second) = &planned[1].model.as_ref().unwrap()[0] else {
            panic!("expected a system message");
        };
        assert_eq!(
            second
                .sections
                .as_ref()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["beta", "alpha"]
        );
    }

    #[test]
    fn tool_changes_ride_on_the_last_planned_entry() {
        let messages = vec![system(
            Some(section_map(&[("alpha", Some("one"))])),
            vec![tool("old", "Old")],
        )];
        let view = ContextView {
            head: None,
            entries: vec![system_entry_record(1)],
            contributions: Vec::new(),
            messages,
        };
        let mut desired = IndexMap::new();
        desired.insert("alpha".to_owned(), "TWO".to_owned());
        let planned = plan_system_entries(&view, &desired, &[tool("new", "New")], 1);
        assert_eq!(planned.len(), 1);
        let Message::System(message) = &planned[0].model.as_ref().unwrap()[0] else {
            panic!("expected a system message");
        };
        assert_eq!(message.tools_removed.as_ref().unwrap()[0].name, "old");
        assert_eq!(message.tools_added.as_ref().unwrap()[0].name, "new");
    }

    #[test]
    fn an_order_change_rewrites_every_tool() {
        let messages = vec![system(None, vec![tool("a", "A"), tool("b", "B")])];
        let view = ContextView {
            head: None,
            entries: vec![system_entry_record(1)],
            contributions: Vec::new(),
            messages,
        };
        let planned = plan_system_entries(
            &view,
            &IndexMap::new(),
            &[tool("b", "B"), tool("a", "A")],
            1,
        );
        assert_eq!(planned.len(), 1);
        let Message::System(message) = &planned[0].model.as_ref().unwrap()[0] else {
            panic!("expected a system message");
        };
        let removed: Vec<&str> = message
            .tools_removed
            .as_ref()
            .unwrap()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(removed, vec!["a", "b"]);
        let added: Vec<&str> = message
            .tools_added
            .as_ref()
            .unwrap()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(added, vec!["b", "a"]);
        assert!(message.sections.is_none());
    }

    #[test]
    fn an_unchanged_plan_is_empty() {
        let messages = vec![system(
            Some(section_map(&[("alpha", Some("one"))])),
            vec![tool("a", "A")],
        )];
        let view = ContextView {
            head: None,
            entries: vec![system_entry_record(1)],
            contributions: Vec::new(),
            messages,
        };
        let mut desired = IndexMap::new();
        desired.insert("alpha".to_owned(), "one".to_owned());
        assert!(plan_system_entries(&view, &desired, &[tool("a", "A")], 1).is_empty());
    }

    // 回归（major）：TS 的 `sections` 是普通对象，`Object.entries`/`Object.fromEntries` 与属性
    // 赋值都按「整数索引键升序在前，其余插入序」枚举；`Map`（Rust 的 IndexMap）只有插入序。
    // 黄金值由 TS 的 `planSystemEntries`/`replaySections` 实测生成。
    #[test]
    fn sections_replay_and_plan_in_js_object_key_order() {
        let messages = vec![system(
            Some(section_map(&[
                ("b", Some("1")),
                ("2", Some("2")),
                ("a", Some("3")),
                ("10", Some("4")),
            ])),
            Vec::new(),
        )];
        let replayed = replay_sections(&messages);
        assert_eq!(
            replayed
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect::<Vec<_>>(),
            vec![("2", "2"), ("10", "4"), ("b", "1"), ("a", "3")]
        );

        // 空上下文的 desired 是插入序；Object.fromEntries 把最小补丁归一成 JS 对象键序。
        let view = ContextView {
            head: None,
            entries: Vec::new(),
            contributions: Vec::new(),
            messages: Vec::new(),
        };
        let mut desired: IndexMap<String, String> = IndexMap::new();
        desired.insert("b".to_owned(), "B".to_owned());
        desired.insert("2".to_owned(), "two".to_owned());
        desired.insert("a".to_owned(), "A".to_owned());
        desired.insert("10".to_owned(), "ten".to_owned());
        let planned = plan_system_entries(&view, &desired, &[], 7);
        assert_eq!(planned.len(), 1);
        let Message::System(message) = &planned[0].model.as_ref().unwrap()[0] else {
            panic!("expected a system message");
        };
        assert_eq!(
            message
                .sections
                .as_ref()
                .unwrap()
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("2", Some("two")),
                ("10", Some("ten")),
                ("b", Some("B")),
                ("a", Some("A"))
            ]
        );

        // 顺序不同时回退为「全部移除 + 全部重加」：重加补丁同样按 JS 对象键序持久化。
        let view = ContextView {
            head: None,
            entries: vec![system_entry_record(1)],
            contributions: Vec::new(),
            messages,
        };
        let planned = plan_system_entries(&view, &desired, &[], 7);
        assert_eq!(planned.len(), 2);
        let Message::System(rewritten) = &planned[1].model.as_ref().unwrap()[0] else {
            panic!("expected a system message");
        };
        assert_eq!(
            rewritten
                .sections
                .as_ref()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["2", "10", "b", "a"]
        );
    }

    // 回归（major）：TS 的最小分段补丁是普通对象属性赋值，`patch["__proto__"] = …` 命中的是
    // `Object.prototype` 的访问器而不是自有属性，所以最小补丁永远改不动名为 `__proto__` 的分段；
    // `Object.fromEntries` 的重写分支与基线则用 CreateDataProperty，保留自有的 `__proto__`。
    // 黄金值全部由 TS 的 `planSystemEntries` 实测生成。
    #[test]
    fn a_minimal_patch_never_touches_a_proto_section() {
        let shown_proto = vec![system(
            Some(section_map(&[("__proto__", Some("a")), ("b", Some("1"))])),
            Vec::new(),
        )];
        let view = ContextView {
            head: None,
            entries: vec![system_entry_record(1)],
            contributions: Vec::new(),
            messages: shown_proto,
        };

        // 改值：TS 返回 []。
        let mut desired = IndexMap::new();
        desired.insert("__proto__".to_owned(), "changed".to_owned());
        desired.insert("b".to_owned(), "1".to_owned());
        assert!(plan_system_entries(&view, &desired, &[], 7).is_empty());

        // 删除：TS 返回 []。
        let mut removed = IndexMap::new();
        removed.insert("b".to_owned(), "1".to_owned());
        assert!(plan_system_entries(&view, &removed, &[], 7).is_empty());

        // 新增（顺序不变，追加在末尾）：TS 返回 []。
        let messages = vec![system(Some(section_map(&[("b", Some("1"))])), Vec::new())];
        let view = ContextView {
            head: None,
            entries: vec![system_entry_record(1)],
            contributions: Vec::new(),
            messages,
        };
        let mut added = IndexMap::new();
        added.insert("b".to_owned(), "1".to_owned());
        added.insert("__proto__".to_owned(), "new".to_owned());
        assert!(plan_system_entries(&view, &added, &[], 7).is_empty());

        // 同时改 `b`：补丁里只有 `b`（TS: `[{"b":"2"}]`）。
        let messages = vec![system(
            Some(section_map(&[("__proto__", Some("a")), ("b", Some("1"))])),
            Vec::new(),
        )];
        let view = ContextView {
            head: None,
            entries: vec![system_entry_record(1)],
            contributions: Vec::new(),
            messages,
        };
        let mut changed = IndexMap::new();
        changed.insert("__proto__".to_owned(), "changed".to_owned());
        changed.insert("b".to_owned(), "2".to_owned());
        let planned = plan_system_entries(&view, &changed, &[], 7);
        assert_eq!(planned.len(), 1);
        let Message::System(message) = &planned[0].model.as_ref().unwrap()[0] else {
            panic!("expected a system message");
        };
        assert_eq!(
            message
                .sections
                .as_ref()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_deref()))
                .collect::<Vec<_>>(),
            vec![("b", Some("2"))]
        );
    }

    // 回归（major）：顺序重写与基线走 `Object.fromEntries`，它用 CreateDataProperty，会保留自有的
    // `__proto__` 键（TS: `[{"b":null,"c":null},{"__proto__":"new","b":"1"}]`）。
    #[test]
    fn from_entries_patches_keep_an_own_proto_key() {
        let messages = vec![system(
            Some(section_map(&[("b", Some("1")), ("c", Some("2"))])),
            Vec::new(),
        )];
        let view = ContextView {
            head: None,
            entries: vec![system_entry_record(1)],
            contributions: Vec::new(),
            messages,
        };
        let mut desired = IndexMap::new();
        desired.insert("__proto__".to_owned(), "new".to_owned());
        desired.insert("b".to_owned(), "1".to_owned());
        let planned = plan_system_entries(&view, &desired, &[], 7);
        assert_eq!(planned.len(), 2);
        let Message::System(first) = &planned[0].model.as_ref().unwrap()[0] else {
            panic!("expected a system message");
        };
        assert_eq!(
            first
                .sections
                .as_ref()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_deref()))
                .collect::<Vec<_>>(),
            vec![("b", None), ("c", None)]
        );
        let Message::System(second) = &planned[1].model.as_ref().unwrap()[0] else {
            panic!("expected a system message");
        };
        assert_eq!(
            second
                .sections
                .as_ref()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_deref()))
                .collect::<Vec<_>>(),
            vec![("__proto__", Some("new")), ("b", Some("1"))]
        );

        // 基线同样保留自有的 `__proto__`（TS: `{"__proto__":"x","b":"1"}`）。
        let baseline_view = ContextView {
            head: Some(EntryRecord {
                head: Some(crate::types::id_from_number(5)),
                ..system_entry_record(99)
            }),
            entries: Vec::new(),
            contributions: Vec::new(),
            messages: Vec::new(),
        };
        let mut baseline_desired = IndexMap::new();
        baseline_desired.insert("__proto__".to_owned(), "x".to_owned());
        baseline_desired.insert("b".to_owned(), "1".to_owned());
        let planned = plan_system_entries(&baseline_view, &baseline_desired, &[], 7);
        assert_eq!(planned.len(), 1);
        let Message::System(message) = &planned[0].model.as_ref().unwrap()[0] else {
            panic!("expected a system message");
        };
        assert_eq!(
            message
                .sections
                .as_ref()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_deref()))
                .collect::<Vec<_>>(),
            vec![("__proto__", Some("x")), ("b", Some("1"))]
        );
    }

    // 回归（major）：TS 的 `systemEntry()` 把 `toolsRemoved` 写在 `toolsAdded` 之前
    // （`harness/prompt.ts` 的对象字面量序），`kiana.system` 条目会逐字节持久化。
    #[test]
    fn tool_changes_serialize_removed_before_added() {
        let messages = vec![system(None, vec![tool("old", "Old")])];
        let view = ContextView {
            head: None,
            entries: vec![system_entry_record(1)],
            contributions: Vec::new(),
            messages,
        };
        let mut desired = IndexMap::new();
        desired.insert("alpha".to_owned(), "one".to_owned());
        let planned = plan_system_entries(&view, &desired, &[tool("new", "New")], 7);
        assert_eq!(planned.len(), 1);
        let message = &planned[0].model.as_ref().unwrap()[0];
        // 落盘走 `persisted_message_json()`（EntryRecord/EntryDraft 的 `model` 字段都经过它）；
        // `SystemMessage` 自身的 serde 序是 TS interface 声明序（ARCHITECTURE §4.3.3）。
        let persisted = crate::types::persisted_message_json(message).unwrap();
        assert_eq!(
            serde_json::to_string(&persisted).unwrap(),
            r#"{"role":"system","content":"","sections":{"alpha":"one"},"toolsRemoved":[{"name":"old"}],"toolsAdded":[{"name":"new","description":"New","parameters":{"type":"object"}}],"timestamp":7}"#
        );
    }
}
