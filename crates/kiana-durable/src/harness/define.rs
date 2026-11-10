//! `harness/define.ts`：让扩展、工具、分段、hook 与包装器带上类型的构造器。
//!
//! TS 里这些大多是**恒等函数**（`defineExtension`、`defineTool`），只为推导泛型；Rust 的
//! 对应物是把 trait 实现装箱成 `Arc<dyn Extension>`，以及构造类型擦除的回调。
//!
//! # 这个文件负责什么
//!
//! 扩展用这里的构造器描述自己的工具/分段/hook/包装器（返回给 [`Extension`] 的对应方法），
//! [`define_extension`] 则把扩展本身装箱后交给 `registry.install`。安装之后各类值的路径不同：
//!
//! - 工具与分段由 `harness/registry.rs` 的 `validate_extension` 校验（工具名在扩展内去重、
//!   section key 合法且非保留、同一扩展内不重复），再由 `harness/agent.rs` 的 `resolve_agent`
//!   合并；
//! - 包装器（[`Wrap`]）不做校验，在 `resolve_agent` 中于合并之后按扩展顺序应用；
//! - hook 不做校验，也不经过 `resolve_agent`：运行期由 `agent_hooks` 按任务名从
//!   `Extension::hooks()` 读取（`harness/agent.rs`）；
//! - 任务名重名由 `RegistryState::new` 与内建任务及其他扩展比较时检查。
//!
//! 最终工具由 `harness/tool.rs` 消费，分段由 `harness/prompt.rs` 消费。本文件只负责形状与
//! 装箱，**不做校验、不注册**：例如分段 key 是否合法，要到 `registry::validate_extension`
//! （安装扩展时）才检查。
//!
//! # 设计原因
//!
//! TS 的 `defineTool<TParameters>` 靠泛型推导参数类型，运行期原样返回对象。Rust 的
//! [`ToolRegistration`] 不携带参数类型参数（见 `harness/types.rs`），无法像 `defineTool` 那样
//! 从 tool 值推导 `TParameters`，因此 [`define_tool`] 只能原样转发；[`define_extension`] 则
//! 额外完成 `E -> Arc<dyn Extension>` 的擦除。[`section`] / [`hook`] / [`wrap_tool`] /
//! [`wrap_section`] 则把闭包与泛型处理器表装进 trait 对象，才能在 registry 里以同一种类型保存。
//!
//! # 注意事项
//!
//! - [`SectionRender`] 的闭包与它返回的 future 都要求 `Send`：它最终成为
//!   `Arc<dyn PromptSection>`，而 [`PromptSection`] 要求 `Send + Sync`。
//! - [`hook`] 擦除的处理器必须与任务定义声明的 `HooksOf<K>` 是**同一具体类型**；任务代码
//!   按自己的类型 downcast，类型不符时这笔 hook 无法被识别。
//! - 这里构造的 [`Wrap`] 只记录目标名字/key；包装发生在 `resolve_agent`，包装器 panic 或
//!   返回改名后的对象都会移除目标并上报（`harness/agent.rs` 的 `apply_wrap`）。

use std::sync::Arc;

use async_trait::async_trait;
use kiana_chord::context::Context;

use crate::harness::types::{
    Extension, HookRegistration, PromptInput, PromptSection, ToolRegistration, Wrap,
};
use crate::types::{BoxError, HooksOf, TaskLike};

/// 把一个扩展实现装箱（TS: `defineExtension`）。
///
/// TS 版是恒等函数，只用来保留字面量的泛型；Rust 侧必须真的装箱：registry 以
/// `Arc<dyn Extension>` 保存扩展，之后对 `tools()` / `sections()` / `hooks()` / `wraps()` /
/// `tasks()` 的每次求值都经过这个 trait 对象。`'static` 约束来自 `Arc<dyn Extension>`
/// （trait object 的默认生命周期）。
pub fn define_extension<E: Extension + 'static>(extension: E) -> Arc<dyn Extension> {
    Arc::new(extension)
}

/// 类型标注用的恒等函数（TS: `defineTool`）。
///
/// 原样返回注册结构；既不校验参数，也不注册。工具真正生效要等它作为 [`Extension::tools`]
/// 的返回项被 registry 收集、再被 `resolve_agent` 解析进 agent。保留这个函数是为了让扩展源码
/// 与 TS 的 `defineTool(...)` 调用点一一对应。
pub fn define_tool(tool: ToolRegistration) -> ToolRegistration {
    tool
}

/// 分段渲染回调。
///
/// 由 [`section`] 包装成 [`PromptSection`]；参数是本次渲染的 [`PromptInput`] 与调用方上下文，
/// 返回 `Ok(None)` 表示本次不显示该分段。返回 `Err` 不会让整个提示失败：`render_sections`
/// 在上下文未 abort 时把它交给上报回调，并沿用上一版该 key 的文本；上下文已 abort 时错误
/// 继续向上传播。
///
/// 用 `Arc<dyn Fn ...>` 而不是函数指针，是因为闭包可以捕获扩展自己的状态；`Send + Sync`
/// 是 [`PromptSection`] 放进 `Arc` 后跨任务使用所要求的约束。
pub type SectionRender = Arc<
    dyn Fn(
            &PromptInput,
            &dyn Context,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Option<String>, BoxError>> + Send>,
        > + Send
        + Sync,
>;

/// `section()` 构造的分段。
///
/// 把擦除后的回调与 key/tag 存在一起；`PromptSection::render` 只是转发到回调。`tag` 保存
/// `Option<bool>`：`None` 表示 TS 对象里没有 `tag` 字段，渲染时按默认行为加标签。
struct FnSection {
    key: String,
    render: SectionRender,
    tag: Option<bool>,
}

#[async_trait]
impl PromptSection for FnSection {
    fn key(&self) -> &str {
        &self.key
    }

    async fn render(
        &self,
        input: &PromptInput,
        context: &dyn Context,
    ) -> Result<Option<String>, BoxError> {
        (self.render)(input, context).await
    }

    fn tag(&self) -> Option<bool> {
        self.tag
    }
}

/// 一段系统提示；默认包成 `<key>...</key>`（TS: `section`）。
///
/// `options` 对应 `{ tag?: boolean }`：`None` 表示缺省（`tag` 为真），`Some(Some(false))`
/// 表示关闭。
///
/// `Some(None)` 与 `None` 等价：两者都让 `FnSection::tag()` 返回 `None`，即 TS 对象里没有
/// `tag` 字段。渲染时 `harness/prompt.rs` 的 `render_sections` 只在 `tag() == Some(false)`
/// 时跳过 `<key>` 包裹，所以缺省行为是“加标签”。
///
/// 这里不校验 key；安装扩展时 `registry::validate_extension` 才要求它匹配
/// `^[a-z][a-z0-9_-]*$` 且不是保留的 `instructions`。闭包在每次构建系统提示时由
/// `render_sections` 调用；返回 `Ok(None)` 表示本次不输出该分段。
pub fn section(
    key: impl Into<String>,
    render: SectionRender,
    options: Option<Option<bool>>,
) -> Arc<dyn PromptSection> {
    Arc::new(FnSection {
        key: key.into(),
        render,
        tag: options.flatten(),
    })
}

/// 任务名匹配 `task` 的 hook 处理器（TS: `hook`）。
///
/// `K` 通常是某个 `Task<I, S, R, H>` 类型，`TaskLike::name` 取出 `definition.name`，所以
/// `task` 参数只用于取名字，处理器本身按值移入。处理器表的类型 `HooksOf<K>` 被擦除成
/// `Arc<dyn Any + Send + Sync>`；任务代码按自己声明的 `Hooks` 类型 downcast 回来，因此同一
/// 任务名下的每个注册都必须使用该任务声明的处理器类型，否则运行期无法识别。
///
/// 这里不校验任务名是否存在：名字对不上任何任务的 hook 不会被 `harness/agent.rs` 的
/// `agent_hooks` 取到。同一扩展可以为同一任务声明多个 hook，它们会按声明顺序全部保留。
pub fn hook<K>(task: &K, handlers: HooksOf<K>) -> HookRegistration
where
    K: TaskLike,
    HooksOf<K>: Send + Sync + 'static,
{
    HookRegistration {
        task: task.name().to_owned(),
        handlers: Arc::new(handlers),
    }
}

/// 包装与 `tool` 同名的工具（TS: `wrapTool`）。
///
/// 目标用**名字**记录，包装在 `resolve_agent` 合并完所有扩展之后执行；同一目标可被多个包装器
/// 按扩展顺序串联，后一个看到前一个的结果。包装器必须返回同名工具：返回改名后的对象或 panic
/// 都会让该工具被移除并上报（`harness/agent.rs` 的 `apply_wrap`），所以包装器不能依赖失败后
/// 目标仍然存在。
pub fn wrap_tool(
    tool: &ToolRegistration,
    wrapper: Arc<dyn Fn(ToolRegistration) -> ToolRegistration + Send + Sync>,
) -> Wrap {
    Wrap::Tool {
        tool: tool.name().to_owned(),
        wrap: wrapper,
    }
}

/// 包装 key 为 `key` 的分段（TS: `wrapSection`）。
///
/// 与 [`wrap_tool`] 同样的规则：按 key 找目标、在合并之后执行；包装器返回的分段必须保持同一个
/// `key()`，否则该分段会被移除并上报。目标不存在时包装器不会被调用。
pub fn wrap_section(
    key: impl Into<String>,
    wrapper: Arc<dyn Fn(Arc<dyn PromptSection>) -> Arc<dyn PromptSection> + Send + Sync>,
) -> Wrap {
    Wrap::Section {
        section: key.into(),
        wrap: wrapper,
    }
}
