//! `harness/agent.ts`：agent 设置、`kiana.agent` 文档与解析出的 agent。
//!
//! TS: `packages/durable/src/harness/agent.ts`。
//!
//! # 这个模块解决什么问题
//! 会话中可配置的 agent 部分（模型、思考等级、扩展与工具选择、追加的 instructions、cwd）
//! 持久化在 `kiana.agent` 文档里。本模块定义该文档，把宿主传入的 [`HarnessSettings`] 解析成
//! 填满默认值的 [`Settings`]，并在需要 agent 时把「已存状态 + registry 快照 + 设置」合成为
//! 一个不可变的 [`Agent`]。
//!
//! # 在系统中的位置
//! - 上游：`harness::harness` 在创建/分叉会话的提交里调用 [`create_agent`]，在 `root()` /
//!   `fork()` 里调用 [`configure`]，在 `Conversation::agent()` 里调用 [`resolve_agent`]。
//! - 下游：`generation` 用解析出的 agent 渲染系统提示、构造请求的工具列表，并调用
//!   [`add_tools`] 与 [`section_extension`]；`tool` 用 [`tool_extension`] 归因错误；
//!   `scheduler` 用 [`agent_hooks`] 收集任务 hook；`registry` 用 [`INSTRUCTIONS_KEY`]
//!   拒绝同名的扩展分段。根模块的 `index` 再导出本模块的常量与 [`configure`]。
//!
//! # 主要流程
//! 1. [`resolve_settings`]：把宿主设置逐字段覆盖到内建默认值上，得到完整的 [`Settings`]。
//! 2. [`configure`] / [`add_tools`] / [`create_agent`]：在调用方的事务里读写 `kiana.agent`
//!    文档草稿；本模块不自己提交事务。
//! 3. [`resolve_agent`]：
//!    - 用 `select_extensions` 选出扩展（已存列表，或对宿主默认选择做 add/remove 编辑）；
//!    - 按扩展顺序合并工具与提示分段（同名时后者胜出），再按扩展顺序应用包装器；
//!    - 用已存的 `tools` 过滤出「已启用」集合，再按 `modelTools` 决定提供给模型的集合；
//!    - 追加 `instructions` 分段，产出 [`Agent`]。
//!
//! # 重要约束
//! - 工具与扩展的选择都按名字匹配，解析时不校验名字当前是否已注册：`add_tools` 会为暂时
//!   下线的工具（例如重连中的 MCP server 提供的工具）先写下名字，等它重新注册后自然生效。
//! - 包装器是用户代码，可能 panic 或改名。[`resolve_agent`] 用 `catch_unwind` 捕获 panic，
//!   丢弃该包装目标并通过 `report` 回调上报，不会让整次解析失败。
//! - [`resolve_agent`] 是同步函数：自身不 `await`、不持锁、不直接做 I/O；但它会执行扩展实现的
//!   `tools()` / `sections()` / `wraps()` 与包装器闭包，并调用 `report`，这些都是外部用户代码，
//!   可能产生任意副作用或 I/O，因此不能视为无副作用的纯函数。调用方必须传入同一时刻的
//!   registry 快照与设置。返回的 [`Agent`] 是快照，registry 之后的变化需要重新解析才体现。
//! - 存储形状是 camelCase 的 JSON（`model`/`thinkingLevel`/`extensions`/`tools`/`modelTools`/
//!   `instructions`/`cwd`），对应 [`AgentState`]；写入集中在 `apply_change`、[`add_tools`] 与
//!   [`create_agent`]，从文档读回并解释的是 [`resolve_agent`]。
//!
//! # 阅读顺序
//! 默认策略常量与 [`AGENT_DOC`] → [`resolve_settings`] → [`configure`]（`apply_change`）→
//! [`add_tools`] → [`create_agent`] → [`resolve_agent`]（`select_extensions` /
//! `filter_tools` / `offered_tools` / `apply_wrap`）→ [`agent_hooks`] /
//! [`section_extension`] / [`tool_extension`]。

use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_chord::context::Context;

use crate::harness::types::{
    Agent, AgentChange, AgentState, CompactionPolicy, ConversationRetryPolicy, Extension,
    ExtensionChange, ExtensionSelection, HarnessSettings, ModelToolChange, ModelToolSelection,
    ProgressPolicy, PromptInput, PromptSection, RegistrySnapshot, ResolvedCompactionSettings,
    Settings, ToolCaller, ToolChange, ToolRegistration, ToolSelection, Wrap,
};
use crate::session::{document_retention, persist_within};
use crate::types::{
    BoxError, ConversationId, ConversationRecord, DocDefinition, DocTarget, DocToken, DocumentFork,
    DocumentSemantics, JsonObject, JsonValue, Tx, define_doc,
};

/// 默认持久重试策略（TS: `DEFAULT_RETRY_POLICY`）。
///
/// [`resolve_settings`] 用它逐字段补齐 `HarnessSettings::retry` 中缺省的部分。三个默认策略
/// 静态量都用 `LazyLock` 延迟构造，字段含义见 [`ConversationRetryPolicy`]。
pub static DEFAULT_RETRY_POLICY: LazyLock<ConversationRetryPolicy> =
    LazyLock::new(|| ConversationRetryPolicy {
        enabled: true,
        max_retries: 3,
        base_delay_ms: 2000.0,
        max_agent_delay_ms: Some(60000.0),
    });

/// 默认自动压缩阈值（TS: `DEFAULT_COMPACTION_POLICY`）。
///
/// [`resolve_settings`] 用它补齐 `HarnessSettings::compaction` 缺省的四个策略字段，结果写进
/// [`ResolvedCompactionSettings`]：数值字段都是具体值，只有 `model_overrides` 仍可选。
pub static DEFAULT_COMPACTION_POLICY: LazyLock<CompactionPolicy> =
    LazyLock::new(|| CompactionPolicy {
        enabled: true,
        reserve_tokens: 16384.0,
        keep_recent_tokens: 20000.0,
        background_tokens: 32768.0,
    });

/// 默认运行进度提交节奏（TS: `DEFAULT_PROGRESS_POLICY`）。
///
/// [`resolve_settings`] 用本常量的 `partial_interval_ms` 与 `output_interval_ms` 填补缺省值；
/// `persist_within_ms` 则走 [`crate::session::persist_within`] 的「非负数字否则默认」规则，
/// 两边默认值同为 1000ms。
pub static DEFAULT_PROGRESS_POLICY: LazyLock<ProgressPolicy> = LazyLock::new(|| ProgressPolicy {
    partial_interval_ms: 100.0,
    output_interval_ms: 100.0,
    persist_within_ms: 1000.0,
});

/// agent `instructions` 的保留分段 key。
///
/// [`resolve_agent`] 在全部扩展分段之后用它追加一个 `InstructionsSection`；安装扩展时
/// [`crate::harness::registry::validate_extension`] 会拒绝 key 等于它的扩展分段，避免两段
/// 提示同名。
pub const INSTRUCTIONS_KEY: &str = "instructions";

/// 默认 `settings.contextRetentionMs`：十分钟。
const DEFAULT_CONTEXT_RETENTION_MS: f64 = 600_000.0;

/// 默认 `settings.restartLimit`。
///
/// `restart_limit` 在值缺失、小于 1 或为 `NaN` 时回落到它；`scheduler` 在宿主设置回调失败时
/// 也用它（见 `scheduler.rs` 的 `restart_limit`）。
pub const DEFAULT_RESTART_LIMIT: f64 = 5.0;

/// 内建 agent 文档；可 rewind，所以 fork 从它们 fork 处的 agent 开始。
///
/// 定义细节：kind 为 `kiana.agent`、version 为 1、会话作用域；
/// `DocumentSemantics::rewindable` 表示保留可 as-of 读取的历史，[`DocumentFork::AsOf`] 表示
/// fork 时在 fork 点初始化副本；初始值是空对象；`checkpoint_when` 恒真，即每次普通变化都按
/// 完整 base 记录（字段含义见 [`DocDefinition`]）。本文件的测试
/// `the_agent_doc_is_rewindable_and_forked_as_of` 锁定这些性质。
///
/// TS: `AgentDoc`
pub static AGENT_DOC: LazyLock<DocToken> = LazyLock::new(|| {
    define_doc(
        DocDefinition::new(
            "kiana.agent",
            1,
            DocumentSemantics::rewindable(DocumentFork::AsOf),
        )
        .with_initial(|_| JsonObject::new())
        .with_checkpoint_when(|_, _, _| true),
    )
});

/// 把 restart 上限解释成重启次数：`1` 或更大时向下取整（`Infinity` 永不泊车），否则用默认。
///
/// `NaN` 走「否则」分支：`>=` 比较对它为假，所以同样回落到 [`DEFAULT_RESTART_LIMIT`]。
fn restart_limit(limit: Option<f64>) -> f64 {
    match limit {
        Some(value) if value >= 1.0 => value.floor(),
        _ => DEFAULT_RESTART_LIMIT,
    }
}

/// 解析宿主设置：每个字段覆盖内建的默认值，对象字段合并。
///
/// TS: `resolveSettings`。`retry` 与 `progress` 是 `Partial<…>`（见
/// [`crate::harness::types::RetrySettings`] / [`crate::harness::types::ProgressSettings`]），
/// 逐字段回落到默认值；`persistWithinMs` 还走 `persistWithin()` 的
/// 「非负数字否则默认」规则。
///
/// `settings` 为 `None` 时全部取默认值。逐项语义：
/// - `extensions` 与 `stream` 不合并：`None` 分别保持 `None` 与空请求选项；
/// - `compaction` 的四个策略字段用 [`DEFAULT_COMPACTION_POLICY`] 逐项补齐，`model_overrides`
///   原样保留（`None` 仍是 `None`）；
/// - `document_retention_ms` 与 `progress.persist_within_ms` 分别经过
///   [`crate::session::document_retention`] / [`crate::session::persist_within`] 的校验，
///   负数与 `NaN` 都回落到默认值；
/// - `restart_limit` 经过 `restart_limit()`：小于 1 或 `NaN` 用 5，`Infinity` 原样保留；
/// - 其余数值字段直接取宿主值或默认值，不做范围校验（负的间隔会原样保留）。
///
/// 本函数不做 I/O、不会失败，返回值总是完整的 [`Settings`]。
pub fn resolve_settings(settings: Option<&HarnessSettings>) -> Settings {
    let extensions = settings.and_then(|settings| settings.extensions.clone());
    let stream = settings
        .and_then(|settings| settings.stream.clone())
        .unwrap_or_default();
    let retry_settings = settings.and_then(|settings| settings.retry.as_ref());
    let retry = ConversationRetryPolicy {
        enabled: retry_settings
            .and_then(|value| value.enabled)
            .unwrap_or(DEFAULT_RETRY_POLICY.enabled),
        max_retries: retry_settings
            .and_then(|value| value.max_retries)
            .unwrap_or(DEFAULT_RETRY_POLICY.max_retries),
        base_delay_ms: retry_settings
            .and_then(|value| value.base_delay_ms)
            .unwrap_or(DEFAULT_RETRY_POLICY.base_delay_ms),
        max_agent_delay_ms: retry_settings
            .and_then(|value| value.max_agent_delay_ms)
            .or(DEFAULT_RETRY_POLICY.max_agent_delay_ms),
    };
    let compaction = settings.and_then(|settings| settings.compaction.as_ref());
    let default_compaction = &*DEFAULT_COMPACTION_POLICY;
    let compaction = ResolvedCompactionSettings {
        enabled: compaction
            .and_then(|value| value.enabled)
            .unwrap_or(default_compaction.enabled),
        reserve_tokens: compaction
            .and_then(|value| value.reserve_tokens)
            .unwrap_or(default_compaction.reserve_tokens),
        keep_recent_tokens: compaction
            .and_then(|value| value.keep_recent_tokens)
            .unwrap_or(default_compaction.keep_recent_tokens),
        background_tokens: compaction
            .and_then(|value| value.background_tokens)
            .unwrap_or(default_compaction.background_tokens),
        model_overrides: compaction.and_then(|value| value.model_overrides.clone()),
    };
    let progress_settings = settings.and_then(|settings| settings.progress.as_ref());
    let progress = ProgressPolicy {
        partial_interval_ms: progress_settings
            .and_then(|value| value.partial_interval_ms)
            .unwrap_or(DEFAULT_PROGRESS_POLICY.partial_interval_ms),
        output_interval_ms: progress_settings
            .and_then(|value| value.output_interval_ms)
            .unwrap_or(DEFAULT_PROGRESS_POLICY.output_interval_ms),
        persist_within_ms: persist_within(
            progress_settings.and_then(|value| value.persist_within_ms),
        ),
    };
    Settings {
        extensions,
        stream,
        retry,
        compaction,
        progress,
        tool_execution: settings
            .and_then(|settings| settings.tool_execution)
            .unwrap_or(crate::harness::types::ToolExecutionMode::Parallel),
        steering_mode: settings
            .and_then(|settings| settings.steering_mode)
            .unwrap_or(crate::harness::types::QueueMode::OneAtATime),
        follow_up_mode: settings
            .and_then(|settings| settings.follow_up_mode)
            .unwrap_or(crate::harness::types::QueueMode::OneAtATime),
        context_retention_ms: settings
            .and_then(|settings| settings.context_retention_ms)
            .unwrap_or(DEFAULT_CONTEXT_RETENTION_MS),
        document_retention_ms: document_retention(
            settings.and_then(|settings| settings.document_retention_ms),
        ),
        after_failed_run: settings
            .and_then(|settings| settings.after_failed_run)
            .unwrap_or(crate::harness::types::AfterFailedRun::Keep),
        restart_limit: restart_limit(settings.and_then(|settings| settings.restart_limit)),
    }
}

/// 把一次变更应用到 `kiana.agent`：给定的字段替换存储值，`Clear` 清空它，缺省什么都不改。
///
/// 通过 `tx.doc` 取出（不存在时创建）该会话的文档草稿并就地修改；草稿随调用方的事务一起
/// 提交或回滚，本函数不自己提交。扩展名/工具名按原样存储，不查 registry 是否存在。
/// `tx.doc` 的错误原样返回，此时变更不完整。
///
/// TS: `configure`
pub async fn configure(
    tx: &dyn Tx,
    conversation_id: ConversationId,
    change: &AgentChange,
) -> Result<(), BoxError> {
    let target = DocTarget::conversation(&AGENT_DOC, conversation_id);
    let mut draft = tx.doc(&target).await?;
    // 全部字段映射集中在 apply_change；草稿是事务内的活值，提交由调用方负责。
    apply_change(draft.value_mut(), change);
    Ok(())
}

/// 工具轮次的 `addTools`：把名字加到 `tools` 与 `modelTools`，让下一次请求提供这些工具。
///
/// 数组缺少的名字逐个追加，`{ remove }` 移除这些名字。未设置的 `tools` 本来就启用全部工具，
/// 所以不写。未设置或对象形态的 `modelTools` 还会把每个名字追加到它的 `add`——这会加载延迟
/// 工具，对其它工具没有影响。按名字处理，不查 registry：此刻没有注册的工具（例如重连中的
/// MCP server 提供的一个）在它再次可用时就被提供。
///
/// 精确语义：
/// - `tools` 是对象时，只有 `remove` 数组命中 `added` 中的名字才重写，且重写后只保留
///   `remove` 键；未设置或其它形态不写；
/// - `modelTools` 不是数组时（对象、未设置或其它 JSON 形态），先删掉 `remove` 中命中的名字，
///   再把缺少的名字按 `added` 的顺序追加到 `add`，整体重写（对象总带 `add`，`remove` 为空时
///   省略该键）；
/// - 名字比较只看字符串元素，数组里的非字符串元素原样保留；`added` 内部的重复名字只会
///   追加一次。
///
/// `generation` 在工具轮次的提交里、仅当累积的 `addTools` 非空时调用本函数，因此这里的写入
/// 与该轮其余边界变更属于同一个事务。除 `tx.doc` 失败外不会返回错误。
///
/// TS: `addTools`
pub async fn add_tools(
    tx: &dyn Tx,
    conversation_id: ConversationId,
    added: &[String],
) -> Result<(), BoxError> {
    let target = DocTarget::conversation(&AGENT_DOC, conversation_id);
    let mut draft = tx.doc(&target).await?;
    let state = draft.value_mut();

    let contains_added = |name: &str| added.iter().any(|added| added == name);
    let push_missing = |names: &mut Vec<JsonValue>, name: &str| {
        if !names.iter().any(|value| value.as_str() == Some(name)) {
            names.push(JsonValue::String(name.to_owned()));
        }
    };

    // `tools`：数组追加缺失的名字；对象只在移除集合命中时重写。
    let replacement = match state.get_mut("tools") {
        Some(JsonValue::Array(tools)) => {
            // 直接改数组；顺序保持原样，新名字追加在末尾。
            for name in added {
                push_missing(tools, name);
            }
            None
        }
        // 对象形态只可能是 `{ remove: [...] }`：把命中的名字从移除集合里去掉。重写后只保留
        // `remove` 键（与 TS 赋新对象的行为一致）。
        Some(JsonValue::Object(tools)) => match tools.get("remove") {
            Some(JsonValue::Array(remove))
                if remove
                    .iter()
                    .any(|v| v.as_str().is_some_and(contains_added)) =>
            {
                let kept: Vec<JsonValue> = remove
                    .iter()
                    .filter(|value| !value.as_str().is_some_and(contains_added))
                    .cloned()
                    .collect();
                let mut object = JsonObject::new();
                object.insert("remove".to_owned(), JsonValue::Array(kept));
                Some(JsonValue::Object(object.into_iter().collect()))
            }
            _ => None,
        },
        _ => None,
    };
    if let Some(replacement) = replacement {
        state.insert("tools".to_owned(), replacement);
    }

    // `modelTools`：数组追加缺失的名字；否则整体重写（等值写入不产生变化）。
    let replacement = match state.get("modelTools") {
        Some(JsonValue::Array(model_tools)) => {
            // 数组是精确列表：新名字追加到末尾；相等值重写不会产生文档变化。
            let mut model_tools = model_tools.clone();
            for name in added {
                push_missing(&mut model_tools, name);
            }
            Some(JsonValue::Array(model_tools))
        }
        other => {
            // 未设置或 `{ remove?, add? }`：`remove` 先过滤，再把缺的名字按 `added` 顺序补进
            // `add`。`add` 已含的名字不会重复追加。
            let (remove, add) = match other {
                Some(JsonValue::Object(model_tools)) => (
                    model_tools
                        .get("remove")
                        .and_then(JsonValue::as_array)
                        .cloned()
                        .unwrap_or_default(),
                    model_tools
                        .get("add")
                        .and_then(JsonValue::as_array)
                        .cloned()
                        .unwrap_or_default(),
                ),
                _ => (Vec::new(), Vec::new()),
            };
            let remove: Vec<JsonValue> = remove
                .into_iter()
                .filter(|value| !value.as_str().is_some_and(contains_added))
                .collect();
            let mut add = add;
            for name in added {
                push_missing(&mut add, name);
            }
            let mut object = JsonObject::new();
            if !remove.is_empty() {
                object.insert("remove".to_owned(), JsonValue::Array(remove));
            }
            object.insert("add".to_owned(), JsonValue::Array(add));
            Some(JsonValue::Object(object.into_iter().collect()))
        }
    };
    if let Some(replacement) = replacement {
        state.insert("modelTools".to_owned(), replacement);
    }
    Ok(())
}

/// 把要落进文档的状态值转成 [`JsonValue`]。
///
/// 入参只会是 `ModelRef` / `ModelThinkingLevel` 这类纯数据：它们都由 serde 派生，预期序列化
/// 不会失败。`expect` 是这条假设的守卫——若将来加入无法序列化的字段，会在调用方的事务回调里
/// 立即 panic，而不是静默写入残缺状态。
fn to_json<T: serde::Serialize>(value: &T) -> JsonValue {
    serde_json::to_value(value).expect("agent state serializes to JSON")
}

/// `set`：`None` 不动，`Some(None)` 删除，`Some(Some(value))` 替换。
fn set_field(state: &mut JsonObject, key: &str, value: Option<Option<JsonValue>>) {
    match value {
        None => {}
        Some(None) => {
            state.shift_remove(key);
        }
        Some(Some(value)) => {
            state.insert(key.to_owned(), value);
        }
    }
}

fn names_of_extensions(items: &[Arc<dyn Extension>]) -> Vec<JsonValue> {
    items
        .iter()
        .map(|extension| JsonValue::String(extension.name().to_owned()))
        .collect()
}

fn names_of_tools(items: &[ToolRegistration]) -> Vec<JsonValue> {
    items
        .iter()
        .map(|tool| JsonValue::String(tool.name().to_owned()))
        .collect()
}

/// 把 [`AgentChange`] 的每个字段写入文档状态（TS 的 `applyChange`）。
///
/// 映射规则：扩展与工具集合只把名字写进文档（`Set` → 名字数组，`Edit`/`Remove` → 只含
/// `add`/`remove` 中已给出的那一侧的对象）；标量字段（model、thinkingLevel、instructions、
/// cwd）直接存值。每个字段都交给 `set_field`，因此外层 `None` 不动、`Some(None)` 删除、
/// `Some(Some(v))` 替换。扩展/工具是否真的存在不在此时校验，留给 `resolve_agent`。
fn apply_change(state: &mut JsonObject, change: &AgentChange) {
    set_field(
        state,
        "model",
        change
            .model
            .as_ref()
            .map(|model| model.as_ref().map(to_json)),
    );
    set_field(
        state,
        "thinkingLevel",
        change
            .thinking_level
            .map(|level| level.map(|level| to_json(&level))),
    );
    // 扩展与工具都只持久化名字；是否安装、是否存在留到 resolve_agent 再判断。
    let extensions = change
        .extensions
        .as_ref()
        .map(|extensions| match extensions {
            ExtensionChange::Clear => None,
            ExtensionChange::Set(items) => Some(JsonValue::Array(names_of_extensions(items))),
            ExtensionChange::Edit { add, remove } => {
                let mut object = JsonObject::new();
                if let Some(add) = add {
                    object.insert("add".to_owned(), JsonValue::Array(names_of_extensions(add)));
                }
                if let Some(remove) = remove {
                    object.insert(
                        "remove".to_owned(),
                        JsonValue::Array(names_of_extensions(remove)),
                    );
                }
                Some(JsonValue::Object(object.into_iter().collect()))
            }
        });
    set_field(state, "extensions", extensions);
    // `Remove` 存成 `{ remove: [...] }`；`Set` 存名字数组；`Clear` 由 set_field 删除该键。
    let tools = change.tools.as_ref().map(|tools| match tools {
        ToolChange::Clear => None,
        ToolChange::Set(items) => Some(JsonValue::Array(names_of_tools(items))),
        ToolChange::Remove { remove } => {
            let mut object = JsonObject::new();
            object.insert(
                "remove".to_owned(),
                JsonValue::Array(names_of_tools(remove)),
            );
            Some(JsonValue::Object(object.into_iter().collect()))
        }
    });
    set_field(state, "tools", tools);
    // `Edit` 的两侧都可缺省：哪侧为 None 就不写哪个键，两侧都缺省时存空对象。
    let model_tools = change
        .model_tools
        .as_ref()
        .map(|model_tools| match model_tools {
            ModelToolChange::Clear => None,
            ModelToolChange::Set(items) => Some(JsonValue::Array(names_of_tools(items))),
            ModelToolChange::Edit { remove, add } => {
                let mut object = JsonObject::new();
                if let Some(remove) = remove {
                    object.insert(
                        "remove".to_owned(),
                        JsonValue::Array(names_of_tools(remove)),
                    );
                }
                if let Some(add) = add {
                    object.insert("add".to_owned(), JsonValue::Array(names_of_tools(add)));
                }
                Some(JsonValue::Object(object.into_iter().collect()))
            }
        });
    set_field(state, "modelTools", model_tools);
    set_field(
        state,
        "instructions",
        change.instructions.as_ref().map(|instructions| {
            instructions
                .as_ref()
                .map(|value| JsonValue::String(value.clone()))
        }),
    );
    set_field(
        state,
        "cwd",
        change
            .cwd
            .as_ref()
            .map(|cwd| cwd.as_ref().map(|value| JsonValue::String(value.clone()))),
    );
}

/// 每个 Harness 创建或 fork 会话的提交里 `kiana.agent` 的内建部分：fork 保留它的 as-of 副本；
/// 新的任务拥有会话复制其拥有者任务会话的已存 agent；新的无拥有者会话从空开始。
///
/// 分支细节：
/// - `parent` 非空（fork）时立即返回：副本由文档的 [`DocumentFork::AsOf`] 语义在 fork 时
///   生成，这里不复制；
/// - 无 owner 的新会话：只把文档物化出来，值是定义里的初始空对象；
/// - 有 owner 的新会话：把 owner 所在会话的 `kiana.agent` 全部键深拷贝进来（`JsonValue` 的
///   `clone` 是深拷贝）。
///
/// 调用点是 `HarnessHooks::conversation_created`：每个创建会话的提交里、四个内建文档之后、
/// 宿主 `conversation_created` 回调之前执行，因此宿主回调仍可覆盖复制来的值。任何一次
/// `tx.doc` 失败都会中断整个提交。
///
/// TS: `createAgent`
pub async fn create_agent(tx: &dyn Tx, conversation: &ConversationRecord) -> Result<(), BoxError> {
    // fork 的 agent 由文档的 as-of 语义生成，不在这里复制。
    if conversation.parent.is_some() {
        return Ok(());
    }
    let target = DocTarget::conversation(&AGENT_DOC, conversation.id);
    let mut agent = tx.doc(&target).await?;
    // 无拥有者：物化空文档即可。
    let Some(owner) = &conversation.owner else {
        return Ok(());
    };
    // 任务拥有的会话：复制 owner 会话的已存 agent。
    let owner_target = DocTarget::conversation(&AGENT_DOC, owner.conversation_id);
    let owner_agent = tx.doc(&owner_target).await?;
    let copy = owner_agent.value().clone();
    let state = agent.value_mut();
    for (key, value) in copy {
        state.insert(key, value);
    }
    Ok(())
}

/// 某个任务名对应的、所选扩展的 hook 处理器，按扩展顺序（TS: `agentHooks`）。
///
/// 一条记录对应一个扩展声明的一个 hook：同一扩展可以为同一任务声明多个 hook，从而产生多条
/// 记录；多个扩展则按 [`Agent::extensions`] 的顺序排列。记录由 [`agent_hooks`] 汇总。
#[derive(Clone)]
pub struct AgentHook {
    /// 提供 hook 的扩展名。
    pub extension: String,
    /// 处理器表（类型擦除）。
    ///
    /// 约定形状是「hook 名 → 处理器」的映射（`IndexMap`/`HashMap`），由任务定义解释为自己的
    /// hook 类型；`scheduler` 在无法识别为映射时会把整个值原样交给任务代码（见
    /// `SchedulerHooks::handlers_with_extension`）。`extension` 用于把该 hook 的失败归因到
    /// 具体扩展。
    pub handlers: Arc<dyn std::any::Any + Send + Sync>,
}

/// 某个任务名对应的、所选扩展的 hook 处理器，按扩展顺序。
///
/// 返回顺序：先按 [`Agent::extensions`] 的顺序，扩展内部再按 `hooks()` 的声明顺序；`task_name`
/// 与 `HookRegistration::task` 精确比较。只读取已解析 agent 里选中的扩展、不访问 registry，
/// 所以调用方拿到新的快照后要先重新解析 agent，才能看到 hook 的变化。
///
/// TS: `agentHooks`
pub fn agent_hooks(agent: &Agent, task_name: &str) -> Vec<AgentHook> {
    let mut handlers = Vec::new();
    for extension in &agent.extensions {
        for hook in extension.hooks() {
            if hook.task == task_name {
                handlers.push(AgentHook {
                    extension: extension.name().to_owned(),
                    handlers: hook.handlers,
                });
            }
        }
    }
    handlers
}

/// `agent` 的某段提示由谁的代码运行：最后一个提供或包装它的所选扩展。
///
/// 「最后一个」指 [`Agent::extensions`] 的迭代顺序：每次命中都会覆盖返回值，所以最后命中的
/// 扩展胜出。命中条件是扩展的 `sections()` 里有同 key 的分段，或 `wraps()` 里有针对该 key
/// 的 [`Wrap::Section`]；这里只读声明，不检查包装器当时是否成功应用。都不命中返回 `None`。
///
/// 注意：`instructions` 分段由 [`resolve_agent`] 在全部包装器之后追加，应用包装器时它还
/// 不在候选集合里，因此不会被包装；但若有扩展声明了 key 为 `instructions` 的
/// [`Wrap::Section`]，本函数仍会把该扩展算作这段提示的提供者。
///
/// TS: `sectionExtension`
pub fn section_extension(agent: &Agent, key: &str) -> Option<String> {
    let mut found = None;
    for extension in &agent.extensions {
        let provides = extension
            .sections()
            .iter()
            .any(|section| section.key() == key);
        let wraps = extension.wraps().iter().any(|wrap| match wrap {
            Wrap::Section { section, .. } => section == key,
            Wrap::Tool { .. } => false,
        });
        if provides || wraps {
            found = Some(extension.name().to_owned());
        }
    }
    found
}

/// `agent` 的某个工具由谁的代码运行：最后一个提供或包装它的所选扩展。
///
/// 与 [`section_extension`] 相同的「最后命中者胜出」规则：命中条件是扩展的 `tools()` 里有
/// 同名工具，或 `wraps()` 里有针对该名字的 [`Wrap::Tool`]；只读声明，不检查包装器是否成功。
/// 都不命中返回 `None`。`tool` 在模型调用的结构化输出校验失败时用本函数把错误归因到扩展；
/// 嵌套调用改为记录 `invalid_structured_output` 诊断。
///
/// TS: `toolExtension`
pub fn tool_extension(agent: &Agent, name: &str) -> Option<String> {
    let mut found = None;
    for extension in &agent.extensions {
        let provides = extension.tools().iter().any(|tool| tool.name() == name);
        let wraps = extension.wraps().iter().any(|wrap| match wrap {
            Wrap::Tool { tool, .. } => tool == name,
            Wrap::Section { .. } => false,
        });
        if provides || wraps {
            found = Some(extension.name().to_owned());
        }
    }
    found
}

/// `tool` 是否已按名字出现在 `offered` 中（用于判断延迟工具是否已经提供）。
fn is_offered(tool: &ToolRegistration, offered: &[ToolRegistration]) -> bool {
    offered.iter().any(|offered| offered.name() == tool.name())
}

/// 从已存状态（缺失：每个字段未设置）、registry 快照与解析后的设置解析出一个 agent。
/// 抛错或改名的包装器丢掉它的目标并被报告；没有目标的包装器什么都不做。
///
/// Rust 侧的包装器函数没有错误通道，因此「抛错」表现为 panic（由 `apply_wrap` 捕获）。
///
/// 输入与输出：
/// - `state`：`kiana.agent` 文档的反序列化结果；`None` 等同所有字段未设置；
/// - `snapshot`：同一时刻的 registry 快照，提供已安装扩展及其工具/分段；
/// - `settings`：只用其中的 `extensions` 作为默认扩展选择；
/// - `report`：包装器失败的出口，参数是错误与包装器所属扩展名；本函数不打印、不向外传播。
///
/// 解析顺序（改这里之前先读 `tests/harness_registry.rs`）：
/// 1. `select_extensions`：已存 `extensions` 是数组就按名字精确选择；否则以
///    `settings.extensions`（缺省用 `snapshot.installed()`）为基底，叠加 `add`、再减去
///    `remove`。名字去重，且只保留能在快照里解析到的扩展。
/// 2. 合并：先按扩展顺序收集全部工具与分段（同名时后一个扩展覆盖前一个，但保留首次出现的
///    位置），再按扩展顺序把包装器作用在合并结果上（同一目标的多个包装器依次串联）。
/// 3. 过滤：`state.tools` 选出「已启用」集合（数组按存储顺序精确选择，`{ remove }` 删除）；
///    再按 `callers` 拆分：不含模型的工具不进入 `tools`/`deferred`，不含工具的不进入
///    `callable`（`callers` 为 `None` 表示两种调用者都允许）。
/// 4. `state.modelTools` 决定模型实际看到的工具：数组精确选择（显式列出的延迟工具也会
///    提供）；否则保留非延迟工具并追加 `add` 中列出的延迟工具。模型可调用、`deferred` 为真
///    但未提供的工具进入 `deferred`。
/// 5. 把 `instructions` 作为最后一段追加为 `InstructionsSection`（不参与包装器）。
///
/// 顺序保证：扩展的顺序决定工具/分段/包装器的处理顺序；`tools` 的数组形式按存储名字顺序
/// 输出，`{ remove }` 与默认形式保持工具的原顺序；延迟工具按 `add` 中的顺序追加。
///
/// 失败模式：只有包装器的 panic/改名会被捕获、移除目标并经 `report` 上报（见 `apply_wrap`）；
/// 扩展 trait 方法 `tools()` / `sections()` / `wraps()` 与调用方传入的 `report` 回调不在捕获
/// 范围内，它们 panic 会直接向上传播。正常路径没有 `Result` 错误通道，所以返回类型不是
/// `Result`。结果是快照而非缓存：registry 或文档变化后必须重新调用。
///
/// TS: `resolveAgent`
pub fn resolve_agent(
    state: Option<&AgentState>,
    snapshot: &dyn RegistrySnapshot,
    settings: &Settings,
    report: &mut dyn FnMut(BoxError, &str),
) -> Agent {
    // 1. 扩展选择：已存选择优先；否则从宿主默认选择（或全部已安装）出发做 add/remove。
    let extensions = select_extensions(
        state.and_then(|state| state.extensions.as_ref()),
        snapshot,
        settings,
    );

    // 2a. 合并工具：同名时后一个扩展覆盖前一个；IndexMap 会保留该名字首次出现的位置。
    let mut composed: IndexMap<String, ToolRegistration> = IndexMap::new();
    for extension in &extensions {
        for tool in extension.tools() {
            composed.insert(tool.name().to_owned(), tool);
        }
    }
    // 2b. 合并提示分段，覆盖规则与工具相同。
    let mut sections: IndexMap<String, Arc<dyn PromptSection>> = IndexMap::new();
    for extension in &extensions {
        for section in extension.sections() {
            sections.insert(section.key().to_owned(), section);
        }
    }
    // 2c. 包装器在合并之后按扩展顺序应用：同一目标被多个包装器依次加工，后一个看到前一个的
    // 结果。失败只会移除该目标并上报，不影响其它条目。
    for extension in &extensions {
        for wrap in extension.wraps() {
            let extension_name = extension.name().to_owned();
            let mut wrap_report = |error: BoxError| report(error, &extension_name);
            match wrap {
                Wrap::Tool { tool, wrap } => {
                    let wrapper = wrap.clone();
                    apply_wrap(
                        &mut composed,
                        &tool,
                        move |value| wrapper(value),
                        |value| value.name().to_owned(),
                        &mut wrap_report,
                    );
                }
                Wrap::Section { section, wrap } => {
                    let wrapper = wrap.clone();
                    apply_wrap(
                        &mut sections,
                        &section,
                        move |value| wrapper(value),
                        |value| value.key().to_owned(),
                        &mut wrap_report,
                    );
                }
            }
        }
    }

    // 3. 存储的 tools 过滤出已启用集合；之后要按 callers 拆成模型侧与工具侧。
    let mut enabled = filter_tools(
        composed.into_values().collect(),
        state.and_then(|state| state.tools.as_ref()),
    );
    // `enabled` 之后还要用两次过滤结果，保留原列表并按调用者筛。
    let callable_by = |caller: ToolCaller, tool: &ToolRegistration| match &tool.callers {
        None => true,
        Some(callers) => callers.contains(&caller),
    };
    // 模型侧：callers 不允许模型的工具不会进入请求。
    let model_callable: Vec<ToolRegistration> = enabled
        .iter()
        .filter(|tool| callable_by(ToolCaller::Model, tool))
        .cloned()
        .collect();
    let tools = offered_tools(
        &model_callable,
        state.and_then(|state| state.model_tools.as_ref()),
    );
    // 模型可调用、deferred 为真且最终没被提供（默认集合与 add 都没有它）的工具单独保留，
    // 供上层知道还有哪些延迟工具可被加载。
    let deferred: Vec<ToolRegistration> = model_callable
        .iter()
        .filter(|tool| tool.deferred && !is_offered(tool, &tools))
        .cloned()
        .collect();
    // 工具侧（嵌套调用）只看 callers，不受 modelTools 过滤影响。
    enabled.retain(|tool| callable_by(ToolCaller::Tools, tool));
    let callable = enabled;

    let instructions = state.and_then(|state| state.instructions.clone());
    let mut agent_sections: Vec<Arc<dyn PromptSection>> = sections.into_values().collect();
    // 5. instructions 固定追加在扩展分段之后；它在包装器阶段之后才出现，不会被扩展包装。
    if let Some(instructions) = &instructions {
        agent_sections.push(Arc::new(InstructionsSection {
            instructions: instructions.clone(),
        }));
    }

    // 未设置的可选字段保持 None；思考等级缺省用 ModelThinkingLevel 的 Default（Off）。
    Agent {
        model: state.and_then(|state| state.model.clone()),
        thinking_level: state
            .and_then(|state| state.thinking_level)
            .unwrap_or_default(),
        extensions,
        tools,
        callable,
        deferred,
        sections: agent_sections,
        instructions,
        cwd: state.and_then(|state| state.cwd.clone()),
    }
}

/// `instructions` 形成的最后一段提示（TS 的 `{ key: INSTRUCTIONS_KEY, render: () => instructions }`）。
///
/// 它不在扩展分段的合并集合里：`resolve_agent` 应用完所有包装器之后才创建并追加它，因此扩展
/// 无法提供或包装这段提示（registry 也不允许扩展分段占用 [`INSTRUCTIONS_KEY`]）。渲染时原样
/// 返回保存的字符串，不读输入；`tag()` 使用 trait 默认值。
struct InstructionsSection {
    instructions: String,
}

#[async_trait]
impl PromptSection for InstructionsSection {
    fn key(&self) -> &str {
        INSTRUCTIONS_KEY
    }

    async fn render(
        &self,
        _input: &PromptInput,
        _context: &dyn Context,
    ) -> Result<Option<String>, BoxError> {
        Ok(Some(self.instructions.clone()))
    }
}

/// 按 `AgentState.modelTools` 描述提供给模型的工具。
///
/// `tools` 是已启用且 `callers` 允许模型的工具（合并后的顺序）。两种存储形态：
/// - 数组：精确列表，交给 `filter_tools`；此时不判断 `deferred`，显式列出的延迟工具也会提供；
/// - `{ remove?, add? }` 或未设置：默认保留所有非延迟工具（去掉 `remove`），再按 `add` 的
///   顺序追加其中尚未提供、未被 `remove` 的延迟工具；`add` 里的非延迟名字不起作用。
///
/// 返回值中先默认集合、后加载的延迟工具，各自保持相对顺序。
fn offered_tools(
    tools: &[ToolRegistration],
    filter: Option<&ModelToolSelection>,
) -> Vec<ToolRegistration> {
    match filter {
        Some(ModelToolSelection::List(list)) => {
            // 精确列表不做延迟判断：显式列出的延迟工具也会提供。
            let selection = ToolSelection::List(list.clone());
            filter_tools(tools.to_vec(), Some(&selection))
        }
        other => {
            // 未设置/增量：先保留默认可见的非延迟工具，再处理 add。
            let (remove, add) = match other {
                Some(ModelToolSelection::Edit { remove, add }) => (
                    remove.clone().unwrap_or_default(),
                    add.clone().unwrap_or_default(),
                ),
                _ => (Vec::new(), Vec::new()),
            };
            let removed: HashSet<&str> = remove.iter().map(String::as_str).collect();
            let mut kept: Vec<ToolRegistration> = tools
                .iter()
                .filter(|tool| !tool.deferred && !removed.contains(tool.name()))
                .cloned()
                .collect();
            let by_name: IndexMap<&str, &ToolRegistration> =
                tools.iter().map(|tool| (tool.name(), tool)).collect();
            // add 只加载延迟工具；非延迟名字即使已被 remove 排除，也不会因出现在 add 中而恢复。
            // 重复名字只加载一次，且 remove 优先于 add。
            let mut seen: HashSet<&str> = HashSet::new();
            for name in &add {
                if !seen.insert(name.as_str()) {
                    continue;
                }
                if let Some(tool) = by_name.get(name.as_str())
                    && tool.deferred
                    && !removed.contains(name.as_str())
                {
                    kept.push((*tool).clone());
                }
            }
            kept
        }
    }
}

/// 经过存储的过滤器：数组按名字、按它的顺序精确选择；`{ remove }` 丢掉名字。
///
/// - `None`：原样返回（保持输入顺序）；
/// - 数组：按存储的名字顺序输出，重复名字只保留第一次，不在输入列表里的名字直接跳过，因此
///   结果可能比输入短，顺序也可能与输入不同；
/// - `{ remove }`：删除命中的名字，其余保持输入顺序。
fn filter_tools(
    tools: Vec<ToolRegistration>,
    filter: Option<&ToolSelection>,
) -> Vec<ToolRegistration> {
    match filter {
        None => tools,
        Some(ToolSelection::List(names)) => {
            // 名字顺序来自存储的选择，而不是工具集合；重复只取第一次，缺失的名字跳过。
            let by_name: IndexMap<&str, &ToolRegistration> =
                tools.iter().map(|tool| (tool.name(), tool)).collect();
            let mut seen: HashSet<&str> = HashSet::new();
            let mut selected = Vec::new();
            for name in names {
                if !seen.insert(name.as_str()) {
                    continue;
                }
                if let Some(tool) = by_name.get(name.as_str()) {
                    selected.push((*tool).clone());
                }
            }
            selected
        }
        Some(ToolSelection::Remove { remove }) => {
            let removed: HashSet<&str> = remove.iter().map(String::as_str).collect();
            tools
                .into_iter()
                .filter(|tool| !removed.contains(tool.name()))
                .collect()
        }
    }
}

/// 选中的已安装扩展：存储的数组，或由 `{ add, remove }` 编辑过的默认选择。
///
/// - 数组：精确名字列表，顺序即存储顺序；
/// - `{ add?, remove? }` 或未设置：基底是 `settings.extensions` 的名字（宿主默认选择），若
///   宿主没给就用 `snapshot.installed()`（安装顺序）；先接上 `add`，再统一减去 `remove`
///   （`remove` 对基底和 `add` 都生效）；
/// - 最后按出现顺序去重，并通过 `snapshot.extension(name)` 解析实例：快照里不存在的名字被
///   跳过，因此选择可以引用尚未安装的扩展，等它安装后重新解析即可生效。
///
/// 名字相同但实例不同的扩展以快照中的那个为准（`settings.extensions` 里的对象只用于取名）。
fn select_extensions(
    stored: Option<&ExtensionSelection>,
    snapshot: &dyn RegistrySnapshot,
    settings: &Settings,
) -> Vec<Arc<dyn Extension>> {
    let selected: Vec<String> = match stored {
        Some(ExtensionSelection::List(list)) => list.clone(),
        other => {
            let (add, remove) = match other {
                Some(ExtensionSelection::Edit { add, remove }) => (
                    add.clone().unwrap_or_default(),
                    remove.clone().unwrap_or_default(),
                ),
                _ => (Vec::new(), Vec::new()),
            };
            let base: Vec<String> = match &settings.extensions {
                Some(extensions) => extensions
                    .iter()
                    .map(|extension| extension.name().to_owned())
                    .collect(),
                None => snapshot
                    .installed()
                    .iter()
                    .map(|extension| extension.name().to_owned())
                    .collect(),
            };
            // add 追加在基底之后；remove 同时作用于基底与 add。
            let removed: HashSet<&str> = remove.iter().map(String::as_str).collect();
            base.into_iter()
                .chain(add)
                .filter(|name| !removed.contains(name.as_str()))
                .collect()
        }
    };
    // 去重并跳过快照里不存在的名字：同一个名字只在首次出现的位置保留。
    let mut seen = HashSet::new();
    let mut extensions = Vec::new();
    for name in selected {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(extension) = snapshot.extension(&name) {
            extensions.push(extension);
        }
    }
    extensions
}

/// 把 `catch_unwind` 捕获的 panic 载荷转成错误。
///
/// `panic!` 的载荷通常是 `&str` 或 `String`，直接取原文；其它类型（例如 `panic_any` 传入
/// 的自定义对象）无法塞进 `BoxError`，统一换成固定文案。
fn panic_error(payload: Box<dyn std::any::Any + Send>) -> BoxError {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).into();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone().into();
    }
    "extension wrapper panicked".into()
}

/// 把 `wrap` 应用到 `items[target]` 上；目标不存在时什么都不做。
///
/// 包装器是扩展提供的用户代码，有两种失败：panic（被 `catch_unwind` 捕获）和返回改名后的
/// 对象（`name_of` 与 `target` 不同）。两种情况都会移除 `items[target]`——后续针对同一目标的
/// 包装器因目标已不存在而跳过——并调用 `report`。成功时按原 key 写回，索引位置不变。
///
/// 这里用 `AssertUnwindSafe` 承担 unwind 风险：捕获期间只操作 `item` 的一份克隆，`items` 只在
/// `catch_unwind` 返回之后才被修改，因此 panic 不会把表留在半更新状态；失败路径随后移除目标。
fn apply_wrap<T: Clone>(
    items: &mut IndexMap<String, T>,
    target: &str,
    wrap: impl Fn(T) -> T,
    name_of: impl Fn(&T) -> String,
    report: &mut dyn FnMut(BoxError),
) {
    let Some(item) = items.get(target).cloned() else {
        return;
    };
    // 用户代码可能 panic：把 panic 归一成上报错误，而不是让整个 resolve_agent 崩溃。
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let wrapped = wrap(item);
        let name = name_of(&wrapped);
        if name != target {
            return Err(format!("Wrapper renamed {target} to {name}"));
        }
        Ok(wrapped)
    }));
    // 只有成功才写回；改名与 panic 都移除目标并上报（扩展名由调用方的闭包补上）。
    match outcome {
        Ok(Ok(wrapped)) => {
            items.insert(target.to_owned(), wrapped);
        }
        Ok(Err(message)) => {
            items.shift_remove(target);
            report(message.into());
        }
        Err(payload) => {
            items.shift_remove(target);
            report(panic_error(payload));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::define::{wrap_section, wrap_tool};
    use crate::harness::types::{HookRegistration, ModelRef};
    use crate::types::CheckpointInfo;
    use kiana_ai::{ModelThinkingLevel, Tool};

    fn tool(name: &str) -> ToolRegistration {
        ToolRegistration::new(Tool {
            name: name.to_owned(),
            description: name.to_owned(),
            parameters: serde_json::json!({"type": "object"}),
            constrained_sampling: None,
        })
    }

    struct TestExtension {
        name: String,
        tools: Vec<ToolRegistration>,
        sections: Vec<Arc<dyn PromptSection>>,
        wraps: Vec<Wrap>,
    }

    impl TestExtension {
        fn new(name: &str) -> Self {
            TestExtension {
                name: name.to_owned(),
                tools: Vec::new(),
                sections: Vec::new(),
                wraps: Vec::new(),
            }
        }
    }

    fn ext(extension: TestExtension) -> Arc<dyn Extension> {
        Arc::new(extension)
    }

    impl Extension for TestExtension {
        fn name(&self) -> &str {
            &self.name
        }
        fn tools(&self) -> Vec<ToolRegistration> {
            self.tools.clone()
        }
        fn sections(&self) -> Vec<Arc<dyn PromptSection>> {
            self.sections.clone()
        }
        fn wraps(&self) -> Vec<Wrap> {
            self.wraps.clone()
        }
        fn hooks(&self) -> Vec<HookRegistration> {
            Vec::new()
        }
    }

    struct TestRegistry {
        extensions: Vec<Arc<dyn Extension>>,
    }

    impl RegistrySnapshot for TestRegistry {
        fn installed(&self) -> Vec<Arc<dyn Extension>> {
            self.extensions.clone()
        }
        fn extension(&self, name: &str) -> Option<Arc<dyn Extension>> {
            self.extensions
                .iter()
                .find(|extension| extension.name() == name)
                .cloned()
        }
        fn tools(&self) -> Vec<crate::harness::types::RegistryTool> {
            Vec::new()
        }
        fn sections(&self) -> Vec<crate::harness::types::RegistrySection> {
            Vec::new()
        }
        fn tasks(&self) -> Vec<crate::harness::types::AnyTask> {
            Vec::new()
        }
        fn task(&self, _name: &str) -> Option<crate::harness::types::AnyTask> {
            None
        }
    }

    fn settings() -> Settings {
        resolve_settings(None)
    }

    #[test]
    fn defaults_match_the_ts_constants() {
        let settings = settings();
        assert!(settings.retry.enabled);
        assert_eq!(settings.retry.max_retries, 3);
        assert_eq!(settings.retry.base_delay_ms, 2000.0);
        assert_eq!(settings.retry.max_agent_delay_ms, Some(60000.0));
        assert!(settings.compaction.enabled);
        assert_eq!(settings.compaction.reserve_tokens, 16384.0);
        assert_eq!(settings.compaction.keep_recent_tokens, 20000.0);
        assert_eq!(settings.compaction.background_tokens, 32768.0);
        assert_eq!(settings.compaction.model_overrides, None);
        assert_eq!(settings.progress.partial_interval_ms, 100.0);
        assert_eq!(settings.progress.output_interval_ms, 100.0);
        assert_eq!(settings.progress.persist_within_ms, 1000.0);
        assert_eq!(settings.context_retention_ms, 600_000.0);
        // TS 的 "resolves settings over the built-in defaults" 用 `expect.any(Number)`：默认文档
        // 保留期可被 `setDefaultDocumentRetention()`（测试夹具）改写，所以不写死 600_000。
        assert_eq!(
            settings.document_retention_ms,
            crate::session::document_retention(None)
        );
        assert_eq!(settings.restart_limit, 5.0);
        assert!(settings.extensions.is_none());
    }

    #[test]
    fn settings_merge_field_by_field() {
        let settings = resolve_settings(Some(&HarnessSettings {
            retry: Some(crate::harness::types::RetrySettings {
                enabled: Some(false),
                max_retries: Some(7),
                base_delay_ms: Some(10.0),
                max_agent_delay_ms: None,
            }),
            compaction: Some(crate::harness::types::CompactionSettings {
                enabled: Some(false),
                reserve_tokens: Some(1.0),
                ..Default::default()
            }),
            progress: Some(crate::harness::types::ProgressSettings {
                partial_interval_ms: Some(5.0),
                output_interval_ms: Some(6.0),
                persist_within_ms: Some(-1.0),
            }),
            restart_limit: Some(2.9),
            ..Default::default()
        }));
        assert!(!settings.retry.enabled);
        assert_eq!(settings.retry.max_retries, 7);
        assert_eq!(settings.retry.max_agent_delay_ms, Some(60000.0));
        assert!(!settings.compaction.enabled);
        assert_eq!(settings.compaction.reserve_tokens, 1.0);
        assert_eq!(settings.compaction.keep_recent_tokens, 20000.0);
        assert_eq!(settings.progress.partial_interval_ms, 5.0);
        assert_eq!(settings.progress.persist_within_ms, 1000.0);
        assert_eq!(settings.restart_limit, 2.0);
    }

    // 回归（major）：TS 的 `HarnessSettings.retry`/`.progress` 是 `Partial<…>`，可以只给一个字段；
    // 其余字段逐项回落到默认值。
    #[test]
    fn partial_retry_and_progress_merge_with_defaults() {
        let settings = resolve_settings(Some(&HarnessSettings {
            retry: Some(crate::harness::types::RetrySettings {
                max_retries: Some(9),
                ..Default::default()
            }),
            progress: Some(crate::harness::types::ProgressSettings {
                output_interval_ms: Some(42.0),
                ..Default::default()
            }),
            ..Default::default()
        }));
        assert!(settings.retry.enabled);
        assert_eq!(settings.retry.max_retries, 9);
        assert_eq!(settings.retry.base_delay_ms, 2000.0);
        assert_eq!(settings.retry.max_agent_delay_ms, Some(60000.0));
        assert_eq!(settings.progress.partial_interval_ms, 100.0);
        assert_eq!(settings.progress.output_interval_ms, 42.0);
        assert_eq!(settings.progress.persist_within_ms, 1000.0);
    }

    #[test]
    fn restart_limit_rounds_down_and_ignores_values_below_one() {
        assert_eq!(restart_limit(None), 5.0);
        assert_eq!(restart_limit(Some(0.0)), 5.0);
        assert_eq!(restart_limit(Some(0.9)), 5.0);
        assert_eq!(restart_limit(Some(1.0)), 1.0);
        assert_eq!(restart_limit(Some(2.9)), 2.0);
        assert_eq!(restart_limit(Some(f64::INFINITY)), f64::INFINITY);
        assert_eq!(restart_limit(Some(f64::NAN)), 5.0);
    }

    #[test]
    fn apply_change_sets_clears_and_ignores() {
        let mut state = JsonObject::new();
        apply_change(&mut state, &AgentChange::default());
        assert!(state.is_empty());

        let change = AgentChange {
            model: Some(Some(ModelRef {
                provider: "p".to_owned(),
                model_id: "m".to_owned(),
            })),
            thinking_level: Some(Some(ModelThinkingLevel::High)),
            extensions: Some(ExtensionChange::Set(vec![ext(TestExtension::new("a"))])),
            tools: Some(ToolChange::Remove {
                remove: vec![tool("read")],
            }),
            model_tools: Some(ModelToolChange::Edit {
                remove: Some(vec![tool("x")]),
                add: Some(vec![tool("y"), tool("z")]),
            }),
            instructions: Some(Some("be nice".to_owned())),
            cwd: Some(Some("/tmp".to_owned())),
        };
        apply_change(&mut state, &change);
        assert_eq!(
            state["model"],
            serde_json::json!({"provider": "p", "modelId": "m"})
        );
        assert_eq!(state["thinkingLevel"], serde_json::json!("high"));
        assert_eq!(state["extensions"], serde_json::json!(["a"]));
        assert_eq!(state["tools"], serde_json::json!({"remove": ["read"]}));
        assert_eq!(
            state["modelTools"],
            serde_json::json!({"remove": ["x"], "add": ["y", "z"]})
        );
        assert_eq!(state["instructions"], serde_json::json!("be nice"));
        assert_eq!(state["cwd"], serde_json::json!("/tmp"));

        let change = AgentChange {
            model: Some(None),
            thinking_level: Some(None),
            extensions: Some(ExtensionChange::Clear),
            tools: Some(ToolChange::Clear),
            model_tools: Some(ModelToolChange::Clear),
            instructions: Some(None),
            cwd: Some(None),
        };
        apply_change(&mut state, &change);
        assert!(state.is_empty());
    }

    #[test]
    fn extension_edits_keep_only_present_keys() {
        let mut state = JsonObject::new();
        apply_change(
            &mut state,
            &AgentChange {
                extensions: Some(ExtensionChange::Edit {
                    add: Some(vec![ext(TestExtension::new("a"))]),
                    remove: None,
                }),
                ..Default::default()
            },
        );
        assert_eq!(state["extensions"], serde_json::json!({"add": ["a"]}));

        apply_change(
            &mut state,
            &AgentChange {
                extensions: Some(ExtensionChange::Edit {
                    add: None,
                    remove: None,
                }),
                ..Default::default()
            },
        );
        assert_eq!(state["extensions"], serde_json::json!({}));
    }

    fn agent_state(value: serde_json::Value) -> AgentState {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn resolve_agent_defaults_to_every_installed_extension() {
        let registry = TestRegistry {
            extensions: vec![
                {
                    let mut extension = TestExtension::new("a");
                    extension.tools = vec![tool("read"), tool("deferred")];
                    extension.tools[1].deferred = true;
                    ext(extension)
                },
                {
                    let mut extension = TestExtension::new("b");
                    extension.tools = vec![tool("write")];
                    ext(extension)
                },
            ],
        };
        let agent = resolve_agent(None, &registry, &settings(), &mut |_, _| {});
        assert_eq!(
            agent
                .extensions
                .iter()
                .map(|extension| extension.name().to_owned())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(
            agent
                .tools
                .iter()
                .map(|tool| tool.name())
                .collect::<Vec<_>>(),
            vec!["read", "write"]
        );
        assert_eq!(
            agent
                .deferred
                .iter()
                .map(|tool| tool.name())
                .collect::<Vec<_>>(),
            vec!["deferred"]
        );
        assert_eq!(agent.thinking_level, ModelThinkingLevel::Off);
    }

    #[test]
    fn model_tools_edit_loads_deferred_tools_in_add_order() {
        let registry = TestRegistry {
            extensions: vec![ext({
                let mut extension = TestExtension::new("a");
                extension.tools = vec![tool("read"), tool("d1"), tool("d2")];
                extension.tools[1].deferred = true;
                extension.tools[2].deferred = true;
                extension
            })],
        };
        let state = agent_state(serde_json::json!({
            "modelTools": {"remove": ["d2"], "add": ["d2", "d1", "read"]}
        }));
        let agent = resolve_agent(Some(&state), &registry, &settings(), &mut |_, _| {});
        assert_eq!(
            agent
                .tools
                .iter()
                .map(|tool| tool.name())
                .collect::<Vec<_>>(),
            vec!["read", "d1"]
        );
        assert_eq!(
            agent
                .deferred
                .iter()
                .map(|tool| tool.name())
                .collect::<Vec<_>>(),
            vec!["d2"]
        );
    }

    #[test]
    fn tools_list_selects_exactly_in_stored_order() {
        let registry = TestRegistry {
            extensions: vec![ext({
                let mut extension = TestExtension::new("a");
                extension.tools = vec![tool("read"), tool("write")];
                extension
            })],
        };
        let state =
            agent_state(serde_json::json!({"tools": ["write", "missing", "write", "read"]}));
        let agent = resolve_agent(Some(&state), &registry, &settings(), &mut |_, _| {});
        assert_eq!(
            agent
                .tools
                .iter()
                .map(|tool| tool.name())
                .collect::<Vec<_>>(),
            vec!["write", "read"]
        );
    }

    #[test]
    fn a_throwing_wrapper_drops_its_target_and_reports_the_extension() {
        let registry = TestRegistry {
            extensions: vec![
                ext({
                    let mut extension = TestExtension::new("provider");
                    extension.tools = vec![tool("read"), tool("keep")];
                    extension
                }),
                ext({
                    let mut extension = TestExtension::new("wrapper");
                    extension.wraps =
                        vec![wrap_tool(&tool("read"), Arc::new(|_tool| panic!("boom")))];
                    extension
                }),
            ],
        };
        let mut reports: Vec<(String, String)> = Vec::new();
        let agent = resolve_agent(None, &registry, &settings(), &mut |error, extension| {
            reports.push((extension.to_owned(), error.to_string()));
        });
        assert_eq!(
            agent
                .tools
                .iter()
                .map(|tool| tool.name())
                .collect::<Vec<_>>(),
            vec!["keep"]
        );
        assert_eq!(reports, vec![("wrapper".to_owned(), "boom".to_owned())]);
    }

    #[test]
    fn a_renaming_wrapper_is_reported_and_dropped() {
        let registry = TestRegistry {
            extensions: vec![
                ext({
                    let mut extension = TestExtension::new("provider");
                    extension.tools = vec![tool("read")];
                    extension
                }),
                ext({
                    let mut extension = TestExtension::new("wrapper");
                    extension.wraps = vec![wrap_tool(
                        &tool("read"),
                        Arc::new(|mut tool| {
                            tool.tool.name = "renamed".to_owned();
                            tool
                        }),
                    )];
                    extension
                }),
            ],
        };
        let mut reports: Vec<(String, String)> = Vec::new();
        let agent = resolve_agent(None, &registry, &settings(), &mut |error, extension| {
            reports.push((extension.to_owned(), error.to_string()));
        });
        assert!(agent.tools.is_empty());
        assert_eq!(
            reports,
            vec![(
                "wrapper".to_owned(),
                "Wrapper renamed read to renamed".to_owned()
            )]
        );
    }

    #[test]
    fn a_wrapper_without_a_target_does_nothing() {
        let registry = TestRegistry {
            extensions: vec![ext({
                let mut extension = TestExtension::new("wrapper");
                extension.wraps = vec![wrap_tool(
                    &tool("missing"),
                    Arc::new(|_tool| panic!("must not run")),
                )];
                extension
            })],
        };
        let mut reported = false;
        let agent = resolve_agent(None, &registry, &settings(), &mut |_, _| reported = true);
        assert!(agent.tools.is_empty());
        assert!(!reported);
    }

    #[test]
    fn instructions_are_appended_as_the_last_section() {
        let registry = TestRegistry {
            extensions: vec![ext({
                let mut extension = TestExtension::new("a");
                extension.sections = vec![crate::harness::define::section(
                    "alpha",
                    Arc::new(|_, _| Box::pin(async { Ok(Some("x".to_owned())) })),
                    None,
                )];
                extension
            })],
        };
        let state = agent_state(serde_json::json!({"instructions": "be nice"}));
        let agent = resolve_agent(Some(&state), &registry, &settings(), &mut |_, _| {});
        assert_eq!(
            agent
                .sections
                .iter()
                .map(|section| section.key().to_owned())
                .collect::<Vec<_>>(),
            vec!["alpha", INSTRUCTIONS_KEY]
        );
        assert_eq!(agent.instructions.as_deref(), Some("be nice"));
    }

    #[test]
    fn section_and_tool_extensions_name_the_last_provider_or_wrapper() {
        let registry = TestRegistry {
            extensions: vec![
                ext({
                    let mut extension = TestExtension::new("provider");
                    extension.tools = vec![tool("read")];
                    extension.sections = vec![crate::harness::define::section(
                        "alpha",
                        Arc::new(|_, _| Box::pin(async { Ok(None) })),
                        None,
                    )];
                    extension
                }),
                ext({
                    let mut extension = TestExtension::new("wrapper");
                    extension.wraps = vec![
                        wrap_tool(&tool("read"), Arc::new(|tool| tool)),
                        wrap_section("alpha", Arc::new(|section| section)),
                    ];
                    extension
                }),
            ],
        };
        let agent = resolve_agent(None, &registry, &settings(), &mut |_, _| {});
        assert_eq!(
            section_extension(&agent, "alpha").as_deref(),
            Some("wrapper")
        );
        assert_eq!(tool_extension(&agent, "read").as_deref(), Some("wrapper"));
        assert_eq!(section_extension(&agent, "missing"), None);
        assert_eq!(tool_extension(&agent, "missing"), None);
    }

    #[test]
    fn agent_hooks_follow_extension_order() {
        // 没有 hook 的扩展返回空列表。
        let agent = Agent {
            model: None,
            thinking_level: ModelThinkingLevel::Off,
            extensions: vec![ext(TestExtension::new("a"))],
            tools: Vec::new(),
            callable: Vec::new(),
            deferred: Vec::new(),
            sections: Vec::new(),
            instructions: None,
            cwd: None,
        };
        assert!(agent_hooks(&agent, "run").is_empty());
    }

    #[test]
    fn the_agent_doc_is_rewindable_and_forked_as_of() {
        let definition = &AGENT_DOC.definition;
        assert_eq!(definition.kind, "kiana.agent");
        assert_eq!(definition.version, 1);
        assert_eq!(
            definition.semantics,
            DocumentSemantics::rewindable(DocumentFork::AsOf)
        );
        assert!((definition.initial)(None).is_empty());
        assert!((definition.checkpoint_when)(
            &JsonObject::new(),
            &[],
            CheckpointInfo {
                deltas_since_base: 0
            }
        ));
    }
}
