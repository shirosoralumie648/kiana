//! `resolve_tool_renderers`：tool / message / entry / markdown 四类渲染器解析链。
//!
//! TS: `core/extensions/runner.ts#getMessageRenderer`、`getMarkdownTransformers`、
//! `resolveToolRenderers`、`getEntryRenderer`。
//!
//! 规范：`spec/ca-extensions.md` §2.3.9、§2.3.13。
//!
//! # 四类解析器
//!
//! - 消息渲染器（[`get_message_renderer`]）：按 `customType` 查每个扩展的
//!   `message_renderers`，返回加载顺序中第一个命中的扩展的条目；
//! - markdown 变换器（[`get_markdown_transformers`]）：每个扩展最多一个，按加载顺序全部返回；
//! - 工具渲染器（[`resolve_tool_renderers`]）：一条可短路的 resolver 链，见下；
//! - 条目渲染器（[`get_entry_renderer`]）：按 `customType` 查 `entry_renderers`（该字段本身
//!   是 `Option`，扩展未注册时为 `None`）。
//!
//! 所有函数都在调用时读取传入的 `extensions` 切片，不缓存结果；扩展集合的顺序就是
//! 加载顺序（`ExtensionTable` 的 `Vec` 顺序，见 `runner` 模块文档）。
//!
//! # 调用方
//!
//! [`ExtensionRunner`](super::ExtensionRunner) 的 `get_message_renderer` /
//! `get_markdown_transformers` / `resolve_tool_renderers` / `get_entry_renderer` 门面方法在
//! 刷新 live 扩展后调用对应函数；`kiana-interactive` 的渲染路径消费返回的渲染器，
//! JS 扩展侧的 resolver 由 loader 的桥接适配（见下文的续延机制）。
//!
//! # 工具渲染器链
//!
//! [`resolve_tool_renderers`] 把每个扩展的 `tool_renderers`（`Option<Vec<...>>`）按加载顺序
//! 展开成一条扁平链。`resolve_at(i)` 调用第 i 个 resolver，并给它一个 `next` 回调；
//! `next()` 递归解析 i+1，链尾调用调用方提供的 `base`。第一个返回 `Some` 的 resolver
//! 短路，后续 resolver 与 `base` 都不执行；所有 resolver 都把 `next()` 的结果原样返回时，
//! 最终结果等于 `base()`（resolver 也可以调用 `next()` 后丢弃结果并返回 `None`）。
//! 解析器列表在调用开始时收集进 `Rc` 快照，已开始的链不会被之后的扩展变更影响。
//!
//! # `next` 的生命周期（为什么有 unsafe）
//!
//! `NextToolRenderersFn` 是 `&dyn Fn`，按契约只在 resolver 的同步调用帧内有效；但 JS
//! 扩展可以把 `next()` 保存下来稍后调用（TS 的闭包天然支持）。为了让 Rust 侧也支持，
//! `resolve_at` 在调用每个 resolver 前把**自有的**续延（捕获了 `Rc` 链快照、工具名与
//! base）压入线程本地栈；loader 的 JS 桥用 `continuation_for` 按地址取走一份 `Rc`，再物化
//! 成 JS 函数。延迟调用因此只访问堆上的链快照，不会解引用已返回的 Rust 栈帧。
//!
//! `base` 只能借用调用方的闭包：`BaseSlot` 用擦除生命周期的指针保存它，并配合
//! `alive: Cell<bool>` 标记调用帧是否仍存活。只有存活时才解引用指针；置为 false 之后，
//! 延迟的 `next()` 读到同步阶段缓存的结果（同步阶段从未求值过 base 则为 `None`）。该做法
//! 依赖扩展运行时是单线程的（设计文档 §2.0），并且 `alive` 在
//! [`resolve_tool_renderers`] 返回前被置为 false。
//!
//! # 失败与边界
//!
//! - resolver panic（TS 的抛错）不会被捕获，直接向上展开；`ContinuationGuard` 在展开时
//!   仍会弹出线程本地栈条目。
//! - 没有扩展、或所有 resolver 都返回 `next()` 的结果时：返回 `base()` 的结果；`base`
//!   返回 `None` 时最终结果也是 `None`。resolver 调用 `next()` 后丢弃结果并返回 `None`
//!   时，最终结果是 `None`（`base` 可能已被求值，但该结果不被采用）。
//! - `next()` 可以重复调用；调用帧存活期间每次都会重新求值 `base` 并更新缓存。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use kiana_ext_api::tool_types::NextToolRenderersFn;
use kiana_ext_api::{
    EntryRenderer, Extension, MarkdownTransformer, MessageRenderer, ToolRendererResolver,
    ToolRenderers,
};

/// 按扩展加载顺序取第一个注册了该 `customType` 的消息渲染器。
///
/// TS: `runner.ts#getMessageRenderer`。`messageRenderers` 是每个扩展的 `Map<string, ...>`，
/// 后注册的同 type 会覆盖同扩展内的前一个（由 loader 保证）。
pub fn get_message_renderer(
    extensions: &[Extension],
    custom_type: &str,
) -> Option<MessageRenderer> {
    extensions
        .iter()
        .find_map(|ext| ext.message_renderers.get(custom_type).cloned())
}

/// 按扩展加载顺序返回全部 markdown 变换器。
///
/// TS: `runner.ts#getMarkdownTransformers`
/// （`extensions.flatMap((ext) => ext.markdownTransformer ? [ext.markdownTransformer] : [])`）。
pub fn get_markdown_transformers(extensions: &[Extension]) -> Vec<MarkdownTransformer> {
    extensions
        .iter()
        .filter_map(|ext| ext.markdown_transformer.clone())
        .collect()
}

/// 解析链上的一个 `next()` 续延：自有的解析器数组 + 工具名 + base 终点。
///
/// JS 侧把 `next` 存起来稍后调用时，这个 `Rc` 让整条链（解析器快照、工具名、已算出的
/// base 结果）保持存活（TS 的闭包同样捕获 `resolvers`/`base`）。
pub(crate) type NextContinuation = Rc<dyn Fn() -> Option<ToolRenderers>>;

thread_local! {
    /// 当前正在同步调用的 resolver 对应的续延（按调用栈压栈）。
    ///
    /// resolver 只从 `resolve_tool_renderers` 拿到 `&NextToolRenderersFn`，安全 Rust 无法把
    /// 借用变成所有权；loader 的 JS 桥在**同一次同步调用**里用 [`continuation_for`] 取走
    /// `Rc` 副本，随后的延迟调用就不再触碰栈帧。
    static ACTIVE_CONTINUATIONS: RefCell<Vec<(usize, NextContinuation)>> = const { RefCell::new(Vec::new()) };
}

/// `next` 指向的续延（仅当它由 [`resolve_tool_renderers`] 创建时）。
///
/// loader 在把 `next` 物化成 JS 函数时调用：拿到自有 `Rc` 后，即使 JS 扩展把函数存到
/// 之后（`setTimeout` 等）再调用也只访问堆上的链快照，不会解引用已返回的 Rust 栈帧。
pub(crate) fn continuation_for(next: &NextToolRenderersFn) -> Option<NextContinuation> {
    let address = next as *const NextToolRenderersFn as *const () as usize;
    ACTIVE_CONTINUATIONS.with(|stack| {
        stack
            .borrow()
            .iter()
            .rev()
            .find(|(known, _)| *known == address)
            .map(|(_, continuation)| Rc::clone(continuation))
    })
}

/// `base` 终点：借用调用方的闭包，仅在解析调用帧内可调用。
///
/// 调用帧结束后（`alive == false`）延迟的 `next()` 不再解引用指针；若 base 在同一次同步
/// 解析里已经求过值则复用缓存（TS 的闭包此时仍可重新调用 base；`&dyn Fn` 无法克隆，
/// 缓存是安全的近似）。
struct BaseSlot {
    /// 调用方提供的 `base`（生命周期被擦除，仅在 `alive` 为 true 时解引用）。
    pointer: &'static dyn Fn() -> Option<ToolRenderers>,
    /// 解析调用帧是否仍然存活。
    alive: Cell<bool>,
    /// 同步调用期间 base 的返回值（延迟调用复用）。
    cached: RefCell<Option<Option<ToolRenderers>>>,
}

impl BaseSlot {
    fn call(&self) -> Option<ToolRenderers> {
        if self.alive.get() {
            // SAFETY: `alive` 只在 `resolve_tool_renderers` 的调用帧结束前为 true；base 引用
            // 来自该帧的 `&dyn Fn`（构造时擦除生命周期）。JS 引擎是单线程的，检查与调用
            // 之间不会跨线程失效。
            let base = self.pointer;
            let value = base();
            *self.cached.borrow_mut() = Some(value.clone());
            return value;
        }
        self.cached.borrow().clone().flatten()
    }
}

/// 一次解析的共享链状态（自有：解析器按 `Arc` 克隆，工具名按值）。
struct Chain {
    resolvers: Vec<ToolRendererResolver>,
    tool_name: String,
    base: BaseSlot,
}

/// 解析工具调用/结果的渲染器：扩展 resolver 按加载顺序运行，最后回落到 `base`。
///
/// TS: `runner.ts#resolveToolRenderers`。`next()` 是惰性的：resolver 可以不调用 `next()`
/// 直接短路返回，也可以 `next() ?? mine`。任意 resolver 抛错会向上传播（无 try/catch）。
///
/// `next` 的续延是堆上自有的（`NextContinuation`）：resolver 可以把它（经 loader 的 JS
/// 桥）保存到调用返回之后，调用时仍按快照的解析器数组继续。
///
/// 参数与返回值：
/// - `extensions`：按加载顺序排列的扩展；每项的 `tool_renderers` 为 `Some` 时其 resolver
///   依序进入链（同一扩展内保持 Vec 顺序）；
/// - `tool_name`：原样传给每个 resolver，供其判断是否处理；
/// - `base`：链尾的兜底渲染器，通常来自已注册工具的 `ToolDefinition`。只有链尾的 `next()`
///   被调用时才求值；扩展列表为空时链上没有任何 resolver，`base` 在本次调用中直接求值
///   （`tests/tool_renderers.rs` 的 `no_extensions_means_base_is_used_directly`）；
/// - 返回链的最外层结果：resolver 调用 `next()` 才会继续向后解析，某个 resolver 不调用
///   `next()` 且返回 `None` 时整体结果就是 `None`（`base` 不会被求值）。
pub fn resolve_tool_renderers(
    extensions: &[Extension],
    tool_name: &str,
    base: &dyn Fn() -> Option<ToolRenderers>,
) -> Option<ToolRenderers> {
    // 链快照：resolver 句柄按加载顺序克隆进 Vec；之后的扩展变更不会影响本次/延迟解析。
    let resolvers: Vec<ToolRendererResolver> = extensions
        .iter()
        .flat_map(|ext| ext.tool_renderers.iter().flatten().cloned())
        .collect();
    let chain = Rc::new(Chain {
        resolvers,
        tool_name: tool_name.to_string(),
        base: BaseSlot {
            // SAFETY: 生命周期被擦除；`BaseSlot::call` 只在 `alive` 为 true（即本调用帧内）
            // 时解引用。`alive` 在本函数返回前置为 false。
            pointer: unsafe {
                std::mem::transmute::<
                    &dyn Fn() -> Option<ToolRenderers>,
                    &'static dyn Fn() -> Option<ToolRenderers>,
                >(base)
            },
            alive: Cell::new(true),
            cached: RefCell::new(None),
        },
    });
    let result = resolve_at(&chain, 0);
    // 调用帧即将结束：之后不允许再解引用 `base`。链本身仍被可能已交给 JS 的 `next`
    // 续延持有（`Rc<Chain>`），所以延迟调用仍能继续走剩余 resolver。
    chain.base.alive.set(false);
    result
}

/// 出栈守卫：resolver panic 时也要把续延从栈上移除。
struct ContinuationGuard;

impl Drop for ContinuationGuard {
    fn drop(&mut self) {
        ACTIVE_CONTINUATIONS.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

/// `resolve(index)`：`index < resolvers.len()` 时调用当前 resolver，否则调用 `base()`。
fn resolve_at(chain: &Rc<Chain>, index: usize) -> Option<ToolRenderers> {
    match chain.resolvers.get(index) {
        Some(resolver) => {
            // 续延捕获的是链快照与下一层下标：即使 resolver 把它保存到本次调用返回之后，
            // 调用时也能从 i+1 继续，不依赖当前栈帧。
            let next_chain = Rc::clone(chain);
            let next: NextContinuation = Rc::new(move || resolve_at(&next_chain, index + 1));
            let address = Rc::as_ptr(&next) as *const () as usize;
            // loader 的 JS 桥用同一个数据指针地址来认领续延；压栈后必须在 resolver 返回时
            // 弹出，`ContinuationGuard` 保证 panic 展开也执行 pop。
            ACTIVE_CONTINUATIONS.with(|stack| {
                stack.borrow_mut().push((address, Rc::clone(&next)));
            });
            let _guard = ContinuationGuard;
            let next_ref: &NextToolRenderersFn = &*next;
            (**resolver)(&chain.tool_name, next_ref)
        }
        None => chain.base.call(),
    }
}

/// 按扩展加载顺序取第一个注册了该 `customType` 的条目渲染器。
///
/// TS: `runner.ts#getEntryRenderer`（`entryRenderers` 可能不存在，Rust 里是
/// `Option<IndexMap<...>>`）。
pub fn get_entry_renderer(extensions: &[Extension], custom_type: &str) -> Option<EntryRenderer> {
    extensions.iter().find_map(|ext| {
        ext.entry_renderers
            .as_ref()
            .and_then(|renderers| renderers.get(custom_type).cloned())
    })
}
