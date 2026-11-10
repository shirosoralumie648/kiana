//! `JsComponent`：JS 组件对象 → `kiana_tui::Component` / `EditorComponent`。
//!
//! 设计文档 §2.9：
//!
//! - `render`/`handleInput`/`handleMouse`/`invalidate`/`wantsKeyRelease` 经 `with_js` 调用；
//! - 每次调用都把对象作为 `This` 传入（§5.2：否则 JS 子类的 `super` 拿不到 `this`）；
//! - 渲染失败返回上一次成功的行（首次失败返回 `[]`），同一组件同一错误只上报一次；
//! - `dispose()` 只由**所有者**显式调用：`ui.custom` 的对话框关闭、widget/footer/header
//!   被替换（[`OwnedJsComponent`]）。普通容器成员（`Box.clear`/`removeChild`、TUI
//!   `removeChild`、overlay `hide`）按 TS 语义**不**调用 `dispose()`（`box.ts:43-46`、
//!   `tui.ts:770-790`），Drop 时只释放 `__kiana_refs` 引用；
//! - `focused` 标志经 [`Focusable`] 转发到 JS 对象（TS `tui.ts:672-680` 的
//!   `isFocusable(c) && (c.focused = …)`）。
//!
//! 本模块同时提供 `ui` 目录共用的 JS 互操作原语：
//!
//! - [`JsEngineSlot`]：线程局部引擎登记（工厂闭包是 `Send + Sync`，不能捕获 `Rc`；
//!   统一在调用时按线程查回引擎，设计文档 §5.6 的 `JsComponent` 说明）；
//! - [`ActiveCtxGuard`]：同一事务内的重入（Rust 原生类的 render 里调用 JS 覆写、JS 样式函数
//!   回调 Rust 组件等）必须复用当前 `Ctx`，绝不能再次 [`JsEngine::with_js`]（§2.2）；
//! - `__kiana_refs`：JS 值引用表。原生类持有 JS 回调用引用 id（不是 `Persistent`），
//!   值的生命周期由 JS 堆管理，引擎销毁时不会留下悬空的 `Persistent`。
//!
//! TS 源：`packages/coding-agent/src/core/extensions/types.ts` 的 `Component` 面。
//!
//! # 为什么用 `__kiana_refs` 而不是 `Persistent`
//!
//! rquickjs 的 `Persistent<Value>` 绑定在创建它的 `Runtime` 上：restore 到别的 runtime
//! 是错误，而它若活过 runtime，rquickjs 会在 `Runtime` 析构时 abort 进程（`Persistent`
//! 的文档要求调用方保证它不越界存活）。引擎自己的 `Drop` 只清空它内部持有的那些
//! `Persistent`（`engine/shutdown.rs`），管不到散落在原生类里的。
//!
//! 原生类与 TUI 组件的存活时间不归引擎管：组件可能在引擎销毁后仍被宿主短暂持有。
//! 把 JS 值存进 `__kiana_refs` 对象、原生类只保存 `u64` id，值的可达性完全由 JS 堆
//! 决定；引擎销毁时整个引用表随上下文释放，不会留下悬空句柄。
//!
//! # 一次 `render` 的执行流程
//!
//! 1. [`Component::render`] 收到 `usize` 宽度，转成 `f64` 再传给 JS（JS 只有 Number）；
//! 2. `with_object` 经 [`with_engine_js`] 取得 `Ctx`：线程上已有 JS 事务就直接复用，
//!    否则在 `Weak` 指向的引擎上开一个新事务并登记 [`ActiveCtxGuard`]；
//! 3. 从引用表取回组件对象，以 `this = 组件对象` 调用 `render(width)`；
//! 4. 返回值是数组（逐元素按 JS `String()` 转换）或 `undefined`/`null`（本次渲染为空）；
//!    其它类型算错误；
//! 5. 成功时把行缓存进 `last_lines`；失败时取走挂起异常、`report` 去重上报，并返回
//!    `last_lines`（首次失败为 `[]`）。
//!
//! # 组件引用的生命周期
//!
//! `JsComponent::reference` 只有两种状态：非 0（拥有 `__kiana_refs` 里的一项）与 0
//! （不再拥有，`reference()` 返回 0）。归零有三条路径，语义不同：
//!
//! - `detach()`：从容器移除；所有权交给调用方，**不**调用 JS `dispose()`，调用方随后
//!   自行 [`drop_ref`]；
//! - `Drop`（内部 `release_reference`）：只释放表项，**不**调用 JS `dispose()`——对应
//!   TS 的普通容器成员（`box.ts:43-46`、`tui.ts:770-790`）；
//! - `dispose()`：所有者显式调用，先调用 JS `dispose()` 再释放表项。
//!
//! [`OwnedJsComponent`] 的 `Drop` 走第三条，[`JsComponent`] 的 `Drop` 走第二条。三条路径
//! 都先把 `reference` 清零再动作，因此重复调用幂等。三个适配器都没有覆写
//! [`Component::dispose`]：宿主通过 trait 调用 `dispose()` 不会到达 JS，JS 侧 `dispose()`
//! 的触发点是 `OwnedJsComponent` 的 Drop 或直接调用 [`JsComponent::dispose`]。
//!
//! # 延迟清理（无事务路径）
//!
//! `dispose()` 与 [`release_ref`] 可能在 JS 事务之外被调用：rquickjs 的类终结器在 JS 堆
//! 释放对象时运行，此时 runtime 的 `RefCell` 已被借用，再进 [`JsEngine::with_js`] 会 panic
//! （设计文档 §2.2）。这类调用只把 id 推入线程局部的延迟队列，真正的清理发生在下一次
//! [`ActiveCtxGuard::enter`]——原生方法入口普遍会调用它，它可能仍位于同一个 JS 事务内部，
//! 而不一定是一个新事务。清理函数用 `pop()` 逐个取走，因此同一批清理的执行顺序与登记
//! 顺序相反；清理过程中新登记的 id 会在同一个循环里处理完。
//!
//! # 失败模式
//!
//! - 组件对象缺失、已释放或不是对象：`with_object` 抛出 JS 异常并转成消息文本；
//! - `render` 返回非数组、`render`/`handleInput`/`handleMouse` 内部抛错：同上；
//! - `report` 用完整消息文本去重（同一文本只上报一次）：先走 JS 的
//!   `__kiana.recordError("ui", message)`（进入 `JsEngine::take_errors`），再写
//!   `tracing::warn!`；
//! - `&self` 访问器（`wants_key_release`/`handles_input`/`focused`/`as_focusable`）与
//!   `get_text` 失败时不调用 `report`，直接返回默认值（`false`/`None`/空串）；
//!   `get_expanded_text` 失败时回退到 `get_text`；`handle_mouse` 的事件序列化失败同样
//!   静默返回 `None`。
//!
//! # 线程约束
//!
//! 引擎登记、事务栈与延迟队列都是 `thread_local`；`Rc<JsEngine>` 与 `Ctx` 只能在创建它们
//! 的线程上使用（设计文档 §2.0）。[`with_engine_js`] 只检查线程上是否有活跃事务，
//! **不**比较该 `Ctx` 与入参 `engine` 是否同一引擎；同一线程上同时存在多个引擎时，
//! 必须避免跨引擎的嵌套事务，否则会读到错误的引用表。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

use kiana_tui::editor_component::{EditorBorderColor, EditorComponent, EditorTextCallback};
use kiana_tui::tui::{Component, Focusable, MouseFocusTarget, TuiMouseEvent, TuiMouseEventResult};
use rquickjs::function::{Args, This};
use rquickjs::{Array, Ctx, Exception, Function, Object, Value};

use crate::engine::{JsEngine, JsError, JsErrorKind, take_exception};

// ===========================================================================
// 线程局部引擎登记
// ===========================================================================

thread_local! {
    /// 当前线程上的引擎（单线程所有权：引擎与 TUI 同线程，§2.0）。
    ///
    /// 按登记顺序保存全部引擎；[`JsEngineSlot::current`] 取最后一个（最后装配的那一代）。
    /// `detach` 只移除指定引擎，因此同一线程上可以并存多个宿主：销毁较新的宿主后，较旧的
    /// 引擎自动重新成为“当前”（`tests/public_surface.rs` 有对应断言）。
    static ENGINE_SLOT: RefCell<Vec<Rc<JsEngine>>> = const { RefCell::new(Vec::new()) };
}

/// 线程局部的引擎句柄登记器。
///
/// `JsEngine` 是 `!Send`：UI 工厂闭包（`Arc<dyn Fn + Send + Sync>`）不能捕获 `Rc<JsEngine>`，
/// 因此在工厂**被调用**时按当前线程查回引擎（设计文档 §5.6 明确要求这条路径）。
pub struct JsEngineSlot;

impl JsEngineSlot {
    /// 登记一个引擎（同一引擎重复登记无副作用）。返回内部 id。
    ///
    /// 返回值是登记表里的位置序号（从 1 开始），**不是稳定的引擎身份**：注销排在前面的
    /// 引擎后，后续引擎的序号会前移。当前仓库内没有把它当作句柄使用的调用方
    /// （`ui::install_attached` 忽略返回值；公开包装 `ui::attach` 仅原样转发），
    /// 不要把它当作可持久化的句柄。
    pub fn attach(engine: &Rc<JsEngine>) -> u64 {
        ENGINE_SLOT.with(|slot| {
            let mut slot = slot.borrow_mut();
            if let Some(existing) = slot
                .iter()
                .position(|candidate| Rc::ptr_eq(candidate, engine))
            {
                return existing as u64 + 1;
            }
            slot.push(Rc::clone(engine));
            slot.len() as u64
        })
    }

    /// 注销一个引擎（引擎销毁时由持有者调用；重复注销无副作用）。
    pub fn detach(engine: &Rc<JsEngine>) {
        ENGINE_SLOT.with(|slot| {
            slot.borrow_mut()
                .retain(|candidate| !Rc::ptr_eq(candidate, engine));
        });
    }

    /// 当前线程最近登记的引擎。
    ///
    /// 语义是“最后登记且仍登记着的引擎”：注销最近的引擎后，前一个登记重新成为当前
    /// （`tests/public_surface.rs` 的 `dropping_a_second_host_restores_the_first_host_engine`）。
    ///
    /// 引擎本身**存放在**这个 TLS 里：线程退出时其析构顺序可能先销毁本 TLS，再销毁引擎
    /// （引擎的 JS 终结器又会回调 `release_ref` 等）。因此这里用 `try_with`，销毁阶段返回
    /// `None` 而不是 panic。
    pub fn current() -> Option<Rc<JsEngine>> {
        ENGINE_SLOT
            .try_with(|slot| slot.borrow().last().cloned())
            .ok()
            .flatten()
    }

    /// 当前线程是否登记了引擎。
    pub fn is_attached() -> bool {
        ENGINE_SLOT
            .try_with(|slot| !slot.borrow().is_empty())
            .unwrap_or(false)
    }
}

// ===========================================================================
// 当前事务的 Ctx（重入安全）
// ===========================================================================

thread_local! {
    /// 当前正在执行的 JS 事务栈。
    ///
    /// `Ctx` 内部是 `NonNull<JSContext>` + 零大小标记；这里只保存**当前线程正在执行**的
    /// 事务的克隆（`Ctx::clone` 会 `JS_DupContext`，Drop 时配对释放），并保证在事务结束前
    /// 全部弹出。作用与「把 Ctx 作为参数层层传递」相同，但让 `Arc<dyn Fn>` 形态的
    /// 主题/边框回调也能在同一事务里同步调用 JS。
    static ACTIVE_CTX: RefCell<Vec<Ctx<'static>>> = const { RefCell::new(Vec::new()) };
}

/// 作用域守卫：进入时登记当前 `Ctx`，离开时弹出。
pub struct ActiveCtxGuard {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ActiveCtxGuard {
    /// 登记当前事务。必须在 `Drop`（作用域结束）前保持存活。
    ///
    /// 进入时还会执行 `flush_pending_disposals`：把在事务外（组件 Drop、JS 终结器）登记的
    /// `dispose` 与引用释放补做完。因此 `kiana-extensions` 的原生方法入口普遍会调用本函数，
    /// 而不是只在 [`with_engine_js`] 开新事务时才调用。
    ///
    /// 守卫内含 `PhantomData<*const ()>`，是 `!Send` 的：事务栈与 JS 值都绑定当前线程。
    /// 它只能包住一次同步的 `Ctx::with` 执行，不能跨 `await` 或线程保存。
    pub fn enter(ctx: &Ctx<'_>) -> ActiveCtxGuard {
        // SAFETY: 值只用于同一事务内的同步重入；`enter`..`drop` 的作用域严格嵌套在
        // `Ctx::with` 里，守卫析构后该指针不再被读取。`Ctx::clone` 增加引用计数。
        let cloned: Ctx<'static> = unsafe { std::mem::transmute(ctx.clone()) };
        ACTIVE_CTX.with(|stack| stack.borrow_mut().push(cloned));
        let guard = ActiveCtxGuard {
            _not_send: std::marker::PhantomData,
        };
        // 事务外被 drop 的组件在这里补做 `dispose()`（终结器里不能开新事务）。
        flush_pending_disposals(ctx);
        guard
    }
}

impl Drop for ActiveCtxGuard {
    fn drop(&mut self) {
        ACTIVE_CTX.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

/// 当前是否有正在执行的事务。
///
/// `dispose`/`release_ref` 用它决定“立即在当前事务里执行”还是“推入延迟队列”：JS 终结器
/// 路径不能开新事务（见 `PENDING_DISPOSALS` 的说明）。
///
/// 引擎在线程退出时由线程局部登记表释放，其终结器可能在本模块的 TLS 已经销毁之后运行；
/// 此时 `try_with` 返回 `AccessError`，按"没有事务"处理（清理动作自然跳过）。
pub fn has_active_ctx() -> bool {
    ACTIVE_CTX
        .try_with(|stack| !stack.borrow().is_empty())
        .unwrap_or(false)
}

thread_local! {
    /// 在 JS 事务之外被 drop 的**所有者组件**引用 id（延迟到下一个事务里 `dispose()` + 释放）。
    ///
    /// rquickjs 的类终结器在 JS 堆释放对象时运行，此时 runtime 的 `RefCell` 已被借用，
    /// 不能再次 [`JsEngine::with_js`]（会 panic）。因此 Drop 只登记，真正清理发生在下一次
    /// `ActiveCtxGuard::enter`（该入口可能是同一个 JS 事务里的另一个原生方法调用，不一定
    /// 是新事务）。引用 id 进程内唯一，跨引擎删除不存在的 id 是无害的空操作。
    static PENDING_DISPOSALS: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    /// 在 JS 事务之外被 drop 的普通组件引用 id（只释放引用表项，**不**调用 `dispose()`）。
    /// 清理时机与 `PENDING_DISPOSALS` 相同（下一次 `ActiveCtxGuard::enter`）。
    static PENDING_REF_DROPS: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

/// JS 事务外被 drop 的所有者组件（widget/footer/header/custom）：登记引用 id，
/// 等待下一次 `ActiveCtxGuard::enter` 清理（`dispose()` + 释放引用）。
pub(crate) fn schedule_disposal(reference: u64) {
    // 线程退出时引擎的终结器可能晚于本模块 TLS 的销毁（见 [`has_active_ctx`]）：丢弃即可。
    let _ = PENDING_DISPOSALS.try_with(|pending| pending.borrow_mut().push(reference));
}

/// JS 事务外被 drop 的普通组件/引用：只登记引用 id，等待下一个事务释放。
pub(crate) fn schedule_ref_drop(reference: u64) {
    let _ = PENDING_REF_DROPS.try_with(|pending| pending.borrow_mut().push(reference));
}

/// 释放一个 `__kiana_refs` 引用（**不**调用 `dispose()`）：当前有事务时立即删除，否则
/// 登记到下一个事务（JS 终结器路径不能开新事务）。
///
/// 线程上没有登记引擎时直接返回：没有引用表可操作，表项随引擎一起消失。
pub fn release_ref(reference: u64) {
    if reference == 0 {
        return;
    }
    let Some(engine) = JsEngineSlot::current() else {
        return;
    };
    if !has_active_ctx() {
        schedule_ref_drop(reference);
        return;
    }
    with_engine_js(&engine, |ctx| drop_ref(&ctx, reference));
}

/// 在当前事务里清理延迟的组件（所有者 `dispose()` + 释放引用；普通组件只释放引用）。
///
/// 两个队列都用 `pop()` 逐个取走，所以同一批清理的执行顺序与登记顺序相反；循环一直跑到
/// 队列为空，清理过程中新登记的 id（JS `dispose()` 可能同步丢弃其它组件）会在同一轮里
/// 处理完。单条 `dispose` 的调用结果被忽略（`let _ = ...`），失败不会中断后续条目。
fn flush_pending_disposals<'js>(ctx: &Ctx<'js>) {
    while let Some(reference) = PENDING_DISPOSALS.with(|pending| pending.borrow_mut().pop()) {
        if let Ok(value) = load_ref(ctx, reference)
            && let Some(object) = value.as_object()
        {
            let _ = call_method::<Value>(ctx, object, "dispose", &[]);
        }
        drop_ref(ctx, reference);
    }
    while let Some(reference) = PENDING_REF_DROPS.with(|pending| pending.borrow_mut().pop()) {
        drop_ref(ctx, reference);
    }
}

/// 在当前事务里执行；没有事务时在引擎上开一个事务。
///
/// 这是「从 Rust 调 JS」的唯一入口：已经处于 JS 事务里时绝不能再次
/// [`JsEngine::with_js`]（设计文档 §2.2，嵌套会因 runtime 的 `RefCell` 已借用而 panic）。
///
/// 两个分支：
/// - 线程上已有活跃事务（`ACTIVE_CTX` 非空）：直接把该 `Ctx` 交给 `f`，不进入引擎、
///   不登记新守卫。JS→Rust→JS 的同步重入走这条路径。
/// - 没有活跃事务：`engine.with_js(...)` 开事务并登记 [`ActiveCtxGuard`]；后者顺带清理
///   延迟队列。
///
/// 注意本函数只按线程判断“有没有事务”，**不**校验活跃 `Ctx` 属于 `engine`；同一线程上
/// 有多个引擎时，调用方必须保证当前事务属于要操作的那个引擎。
pub fn with_engine_js<R>(engine: &Rc<JsEngine>, f: impl for<'js> FnOnce(Ctx<'js>) -> R) -> R {
    // 线程退出阶段该 TLS 可能已销毁（见 [`JsEngineSlot::current`]）：按“无活动事务”处理。
    let active = ACTIVE_CTX
        .try_with(|stack| stack.borrow().last().cloned())
        .ok()
        .flatten();
    match active {
        Some(ctx) => f(ctx),
        None => engine.with_js(|ctx| {
            let _active = ActiveCtxGuard::enter(&ctx);
            f(ctx)
        }),
    }
}

// ===========================================================================
// `__kiana_refs`：JS 值引用表
// ===========================================================================

static NEXT_REF: AtomicU64 = AtomicU64::new(1);

/// `__kiana_refs` 全局对象（由 [`crate::ui::install`] 创建）。
///
/// 是普通 JS 对象，属性名是十进制引用 id。UI 层未安装时全局不存在，这里返回 `Err`，
/// 调用方通常直接 `?` 传播。
pub fn js_refs<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    ctx.globals().get("__kiana_refs")
}

/// 保存一个 JS 值，返回引用 id。
///
/// id 由进程级 `AtomicU64`（从 1 开始）分配，只在**同一个引擎**的 `__kiana_refs` 里有
/// 意义：查找总是发生在某个引擎自己的上下文里，编号跨引擎递增也不影响正确性。
/// `0` 被 `JsComponent`/`release_ref` 用作“无引用”哨兵，本函数永不返回 0。
///
/// 表项是 JS 对象上的普通属性，持有值的强引用，直到 [`drop_ref`] 或被引擎销毁。
pub fn store_ref<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<u64> {
    let refs = js_refs(ctx)?;
    let id = NEXT_REF.fetch_add(1, Ordering::Relaxed);
    refs.set(id.to_string(), value)?;
    Ok(id)
}

/// 读取引用；不存在时返回 `undefined`。
///
/// 只读，不移除表项；id 从未分配、已 [`drop_ref`] 或属于另一个引擎时都得到 `undefined`
/// 而**不是**错误。需要“引用必须存在”的调用方要自行判断（例如 `with_object` 取不到
/// 对象时会抛错）。
///
/// 实现用 `Option<Value>` 检测“属性缺失”，而 rquickjs 的 `Option` 把 `null` 与
/// `undefined` 都视为空；因此表项里存的若是 JS `null`，读出来也是 `undefined`。
pub fn load_ref<'js>(ctx: &Ctx<'js>, id: u64) -> rquickjs::Result<Value<'js>> {
    let refs = js_refs(ctx)?;
    match refs.get::<_, Option<Value<'js>>>(id.to_string())? {
        Some(value) => Ok(value),
        None => Ok(Value::new_undefined(ctx.clone())),
    }
}

/// 删除引用（重复删除无副作用）。
///
/// 引用表不存在（UI 层未安装/引擎销毁中）或属性删除失败都静默忽略：调用点通常在 `Drop`
/// 或清理路径上，没有可恢复动作。
pub fn drop_ref<'js>(ctx: &Ctx<'js>, id: u64) {
    if let Ok(refs) = js_refs(ctx) {
        let _ = refs.remove(id.to_string());
    }
}

/// 在引擎上删除引用（Drop 里使用）。
///
/// 若当前正处于 JS 事务，直接复用当前 `Ctx`；否则开一个事务。引擎已销毁时静默忽略
/// （JS 堆随 runtime 一起释放）。
pub fn drop_ref_on_engine(engine: &Rc<JsEngine>, id: u64) {
    let _ = with_engine_js(engine, |ctx| -> rquickjs::Result<()> {
        drop_ref(&ctx, id);
        Ok(())
    });
}

// ===========================================================================
// 调用辅助
// ===========================================================================

/// 一个 JS 调用参数。
#[derive(Clone, Debug)]
pub enum JsArg {
    /// `undefined`
    Undefined,
    /// `null`
    Null,
    /// 布尔
    Bool(bool),
    /// 数字（JS 的 Number）
    Number(f64),
    /// 字符串
    String(String),
    /// JSON 值（对象按键序物化）
    Json(serde_json::Value),
    /// 已保存的 JS 引用
    Ref(u64),
}

impl JsArg {
    fn into_js<'js>(self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
        match self {
            JsArg::Undefined => Ok(Value::new_undefined(ctx.clone())),
            JsArg::Null => Ok(Value::new_null(ctx.clone())),
            JsArg::Bool(value) => Ok(Value::new_bool(ctx.clone(), value)),
            JsArg::Number(value) => Ok(Value::new_number(ctx.clone(), value)),
            JsArg::String(value) => {
                Ok(rquickjs::String::from_str(ctx.clone(), &value)?.into_value())
            }
            JsArg::Json(value) => json_to_value(ctx, &value),
            JsArg::Ref(id) => load_ref(ctx, id),
        }
    }

    /// 便捷构造：`&str`。
    pub fn str(value: impl Into<String>) -> JsArg {
        JsArg::String(value.into())
    }

    /// 便捷构造：f64。
    pub fn num(value: impl Into<f64>) -> JsArg {
        JsArg::Number(value.into())
    }
}

/// `serde_json::Value` → JS 值（对象键序保留；依赖 `serde_json` 的 `preserve_order`，
/// 工作区已在 `Cargo.toml` 打开该特性）。
///
/// 数字统一物化成 JS Number（`serde_json::Number::as_f64`）；数组按下标 `set`，对象按
/// 迭代顺序逐键 `set`。
pub fn json_to_value<'js>(
    ctx: &Ctx<'js>,
    value: &serde_json::Value,
) -> rquickjs::Result<Value<'js>> {
    match value {
        serde_json::Value::Null => Ok(Value::new_null(ctx.clone())),
        serde_json::Value::Bool(value) => Ok(Value::new_bool(ctx.clone(), *value)),
        serde_json::Value::Number(number) => Ok(Value::new_number(
            ctx.clone(),
            number.as_f64().unwrap_or(f64::NAN),
        )),
        serde_json::Value::String(text) => {
            Ok(rquickjs::String::from_str(ctx.clone(), text)?.into_value())
        }
        serde_json::Value::Array(items) => {
            let array = Array::new(ctx.clone())?;
            for (index, item) in items.iter().enumerate() {
                array.set(index, json_to_value(ctx, item)?)?;
            }
            Ok(array.into_value())
        }
        serde_json::Value::Object(object) => {
            let result = Object::new(ctx.clone())?;
            for (key, item) in object {
                result.set(key.as_str(), json_to_value(ctx, item)?)?;
            }
            Ok(result.into_value())
        }
    }
}

/// JS 值 → `serde_json::Value`（`undefined` → `null`）。
///
/// 语义就是 `JSON.stringify`：顶层 `undefined`/函数/Symbol 得到 `None`（→ `null`）；
/// 循环引用等让 `JSON.stringify` 抛错的情况以 `Err` 返回；字符串化成功但反序列化失败时
/// 回退 `Null`（`unwrap_or`）。
pub fn value_to_json<'js>(
    ctx: &Ctx<'js>,
    value: Value<'js>,
) -> rquickjs::Result<serde_json::Value> {
    match ctx.json_stringify(value)? {
        Some(text) => {
            let text = text.to_string()?;
            Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
        }
        None => Ok(serde_json::Value::Null),
    }
}

/// 读取 JS 数组为 `Vec<String>`（非字符串元素按 JS `String()` 语义转换）。
///
/// 下限是 0、上限在进入循环时取一次 `Array::len`。数组空洞读出来是 `undefined`，
/// 按 `String(undefined)` 变成字符串 `"undefined"`。
pub fn strings_from_array<'js>(array: &Array<'js>) -> rquickjs::Result<Vec<String>> {
    let mut result = Vec::with_capacity(array.len());
    for index in 0..array.len() {
        let value: Value<'js> = array.get(index)?;
        result.push(js_string(&value)?);
    }
    Ok(result)
}

/// `String(value)`。
///
/// 每次调用都从 `globalThis` 重新取 `String` 函数，因此扩展若覆盖了全局 `String`，
/// 这里的转换结果也随之改变；对象值走 JS 自己的 `toString`/`valueOf`。
pub fn js_string<'js>(value: &Value<'js>) -> rquickjs::Result<String> {
    let ctx = value.ctx();
    let string: rquickjs::String<'js> = ctx
        .globals()
        .get::<_, Function<'js>>("String")?
        .call((value.clone(),))?;
    string.to_string()
}

/// 以 `this = receiver` 调用一个函数值。
///
/// `receiver = None` 时以 `this = undefined` 调用（`Args::new` 的初始值）；参数按传入顺序
/// 入栈。`this` 或参数的 `IntoJs` 出错时提前返回 `Err`，函数不会被调用。
pub fn call_with_this<'js, R>(
    ctx: &Ctx<'js>,
    function: &Function<'js>,
    receiver: Option<Value<'js>>,
    args: Vec<Value<'js>>,
) -> rquickjs::Result<R>
where
    R: rquickjs::FromJs<'js>,
{
    let mut call_args = Args::new(ctx.clone(), args.len());
    if let Some(receiver) = receiver {
        call_args.this(receiver)?;
    }
    for arg in args {
        call_args.push_arg(arg)?;
    }
    call_args.apply(function)
}

/// 调用引用 `id` 指向的函数；`this` 由 `receiver` 指定。
///
/// 引用不存在（含已释放）或指向的不是函数时抛 JS 异常（消息 `value is not a function`），
/// 以 `Err` 返回，不区分这两种情况；需要区分时先自行 [`load_ref`] 检查。
pub fn call_ref<'js, R>(
    ctx: &Ctx<'js>,
    id: u64,
    receiver: Option<Value<'js>>,
    args: &[JsArg],
) -> rquickjs::Result<R>
where
    R: rquickjs::FromJs<'js>,
{
    let value = load_ref(ctx, id)?;
    let Some(function) = value.as_function() else {
        return Err(Exception::throw_message(ctx, "value is not a function"));
    };
    let mut js_args = Vec::with_capacity(args.len());
    for arg in args {
        js_args.push(arg.clone().into_js(ctx)?);
    }
    call_with_this(ctx, function, receiver, js_args)
}

/// 调用引用 `id` 指向的函数，参数是现成的 JS 值（用于把新构造的代理对象传给 JS 工厂）。
pub fn call_ref_with_values<'js>(
    ctx: &Ctx<'js>,
    id: u64,
    receiver: Option<Value<'js>>,
    args: Vec<Value<'js>>,
) -> rquickjs::Result<Value<'js>> {
    let value = load_ref(ctx, id)?;
    let Some(function) = value.as_function() else {
        return Err(Exception::throw_message(ctx, "value is not a function"));
    };
    call_with_this(ctx, function, receiver, args)
}

/// 通过 `this` 对象调用 `name` 方法；方法缺失时返回 `Ok(None)`。
///
/// “缺失”包括属性不存在与属性存在但不是函数，两者都返回 `Ok(None)`（TS 的可选方法
/// `obj.name?.(...)` 语义）。`Ok(Some(_))` 中的返回值经 `R::from_js` 转换，转换失败则
/// 返回 `Err`。
pub fn call_method<'js, R>(
    ctx: &Ctx<'js>,
    receiver: &Object<'js>,
    name: &str,
    args: &[JsArg],
) -> rquickjs::Result<Option<R>>
where
    R: rquickjs::FromJs<'js>,
{
    let member: Value<'js> = receiver.get(name)?;
    let Some(function) = member.as_function() else {
        return Ok(None);
    };
    let mut js_args = Vec::with_capacity(args.len());
    for arg in args {
        js_args.push(arg.clone().into_js(ctx)?);
    }
    let value =
        call_with_this::<Value<'js>>(ctx, function, Some(receiver.clone().into_value()), js_args)?;
    Ok(Some(R::from_js(ctx, value)?))
}

/// 读取 JS 对象上的可选字段（缺失与 `undefined` 都返回 `None`）。
///
/// `null` 也折叠为 `None`；其它类型按 JS `String()` 转换（对象走 `toString`）。
pub fn opt_string<'js>(object: &Object<'js>, key: &str) -> rquickjs::Result<Option<String>> {
    let value: Value<'js> = object.get(key)?;
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    Ok(Some(js_string(&value)?))
}

/// 读取 JS 对象上的数字字段（按 JS `Number()` 语义）。
///
/// 缺失、`undefined` 与 `null` 都返回 `None`——注意 `null` **不会**像 JS `Number(null)`
/// 那样变成 0，而是与“未提供”等价。
pub fn opt_number<'js>(object: &Object<'js>, key: &str) -> rquickjs::Result<Option<f64>> {
    let value: Value<'js> = object.get(key)?;
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    let ctx = object.ctx();
    let number: f64 = ctx
        .globals()
        .get::<_, Function<'js>>("Number")?
        .call((value,))?;
    Ok(Some(number))
}

/// 读取 JS 对象上的布尔字段（按 JS truthiness）。
///
/// 缺失、`undefined` 与 `null` 都返回 `None`；其余值经 [`js_truthy`] 转成布尔
/// （`0`、空串为假，对象/函数为真）。
pub fn opt_bool<'js>(object: &Object<'js>, key: &str) -> rquickjs::Result<Option<bool>> {
    let value: Value<'js> = object.get(key)?;
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    Ok(Some(js_truthy(&value)))
}

/// JS `Boolean(value)`（TO-BOOL 抽象操作）。
///
/// `undefined`/`null` 为假（调用方通常已折叠成 `None`）；`0`、`-0`、`NaN`、空串为假；
/// 其余数字/字符串以及对象、函数、Symbol、BigInt 为真。
///
/// 实现只检查布尔、数字、字符串三种原生类型，其余一律落到最后的 `true`。因此 BigInt
/// `0n` 在这里为真，而 ECMAScript 的 TO-BOOL 规定它为假；其余调用点读取的都是 TS 里
/// 声明为布尔/数字的字段（或如 `TuiProxy::unfocus` 用它判定整个 `options` 对象的真值），
/// 只有扩展传入异常类型时才会碰到这个差异。
pub fn js_truthy(value: &Value<'_>) -> bool {
    if let Some(flag) = value.as_bool() {
        return flag;
    }
    if let Some(number) = value.as_number() {
        return number != 0.0 && !number.is_nan();
    }
    if let Some(text) = value.as_string() {
        return text
            .to_string()
            .map(|text| !text.is_empty())
            .unwrap_or(true);
    }
    true
}

// ===========================================================================
// JsComponent
// ===========================================================================

/// JS `handleMouse` 的返回值 → [`TuiMouseEventResult`]。
///
/// TS 的 `focusTarget` 是组件引用（`tui.ts:81-104`），Rust 的组件对象不能跨回宿主的挂载树；
/// 直接 serde 反序列化会因 `focusTarget` 是对象而失败（`.ok()` 把整个结果丢掉，连
/// `handled`/`focus` 一起丢失）。这里逐字段按 JS truthiness 读取，并把对象形式的
/// `focusTarget` 解释为“本次接收者”（[`MouseFocusTarget::SelfComponent`]，由最外层分发者
/// 用 fallback 目标解析成实际组件）。
///
/// 结果对象本身缺失/为 `null`/不是对象时返回 `Ok(None)`（当作“未处理”）。对象存在时
/// 每个字段独立读取：缺失或 `null` 留 `None`（区别于显式的 `false`），其余值经
/// [`js_truthy`] 转成 `Some(bool)`。`focusTarget` 只有是对象时才记为 `SelfComponent`，
/// 其它类型（字符串/数字等）都当作没有指定目标。
fn mouse_result_from_js(value: Value<'_>) -> rquickjs::Result<Option<TuiMouseEventResult>> {
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    let field = |name: &str| -> rquickjs::Result<Option<bool>> {
        let value: Value = object.get(name)?;
        if value.is_undefined() || value.is_null() {
            Ok(None)
        } else {
            Ok(Some(js_truthy(&value)))
        }
    };
    let focus_target: Value = object.get("focusTarget")?;
    Ok(Some(TuiMouseEventResult {
        handled: field("handled")?,
        capture: field("capture")?,
        focus: field("focus")?,
        render: field("render")?,
        focus_target: if focus_target.is_object() {
            Some(MouseFocusTarget::SelfComponent)
        } else {
            None
        },
    }))
}

/// 把 JS 组件对象实现为 [`Component`]。
///
/// `engine` 是 `Weak`：组件本身可能被 TUI 持有到引擎销毁之后（例如关闭流程里仍在渲染），
/// 此时渲染返回上一次成功的行。
pub struct JsComponent {
    /// 组件对象在 `__kiana_refs` 里的引用 id（0 表示已释放）。
    reference: u64,
    /// 同线程引擎（仅用于开事务）。
    engine: Weak<JsEngine>,
    /// 上一次成功渲染的行（设计文档 §2.9 的兜底）。
    last_lines: Vec<String>,
    /// 已上报的错误文本（同一错误只上报一次）。
    reported: HashSet<String>,
    /// 与本组件生命周期绑定的附加 `__kiana_refs` 引用（例如子菜单工厂的 `done` 回调）。
    /// 组件 Drop/`dispose()` 时一并释放，避免每次打开子菜单都留下无法回收的函数。
    extra_refs: Vec<u64>,
}

impl JsComponent {
    /// 从引用 id 构造（组件对象已保存在 `__kiana_refs`）。
    ///
    /// 不校验 `reference` 是否指向现存对象：id 为 0 或已被释放时，错误要到第一次真实调用
    /// 才出现——`render` 等 `&mut self` 路径会 `report`，只读访问器则静默返回默认值。
    pub fn from_ref(engine: &Rc<JsEngine>, reference: u64) -> Self {
        JsComponent {
            reference,
            engine: Rc::downgrade(engine),
            last_lines: Vec::new(),
            reported: HashSet::new(),
            extra_refs: Vec::new(),
        }
    }

    /// 绑定一个附加引用：组件 Drop 时释放（不调用 `dispose()`）。
    pub fn attach_ref(&mut self, reference: u64) {
        if reference != 0 {
            self.extra_refs.push(reference);
        }
    }

    /// 从 JS 对象构造：先存入 `__kiana_refs`，再建立组件。
    ///
    /// **不变量**：`object` 必须来自 `engine` 的同一个 JS 上下文。引用 id 只在该引擎的
    /// `__kiana_refs` 里有意义；把 A 引擎的对象配上 B 引擎，后续调用会得到“引用不存在”。
    pub fn from_object<'js>(engine: &Rc<JsEngine>, object: Object<'js>) -> rquickjs::Result<Self> {
        let ctx = object.ctx().clone();
        let reference = store_ref(&ctx, object.into_value())?;
        Ok(JsComponent::from_ref(engine, reference))
    }

    /// 引用 id（`0` = 已释放）。
    pub fn reference(&self) -> u64 {
        self.reference
    }

    /// 从宿主树里摘除时调用：释放本组件对引用表的**所有权**，但**不**调用 JS `dispose()`。
    ///
    /// TS 的 `Container.removeChild` 只是 `splice` 掉数组项，组件对象仍然存活、可以再挂载；
    /// `dispose()` 只由对话框/overlay 的所有者显式调用。调用方随后需要自行
    /// [`drop_ref`] 释放引用表项（组件本身不再引用它）。
    ///
    /// 清零后本对象不再持有任何引用：之后再调用它按“引用不存在”处理（`with_object`
    /// 取不到对象，渲染会退回 `last_lines`）。
    pub fn detach(&mut self) {
        self.reference = 0;
    }

    /// 释放本组件持有的 `__kiana_refs` 引用（**不**调用 JS `dispose()`）。
    ///
    /// TS 的普通容器成员（`Box.clear`/`removeChild`、TUI `removeChild`、overlay `hide`）
    /// 在垃圾回收时只让对象变得不可达，从不调用 `dispose()`（`box.ts:43-46`、
    /// `tui.ts:770-790`）。Rust 侧对应的时刻就是这里的 Drop。
    fn release_reference(&mut self) {
        let reference = self.reference;
        self.reference = 0;
        self.release_extra_reference(reference);
        self.release_extra_refs();
    }

    /// 释放所有附加引用。
    fn release_extra_refs(&mut self) {
        let references: Vec<u64> = std::mem::take(&mut self.extra_refs);
        for reference in references {
            self.release_extra_reference(reference);
        }
    }

    /// 释放一个附加的 `__kiana_refs` 引用（属性回调等，不调用 `dispose()`）。
    ///
    /// 引擎已销毁时直接返回（引用表随 runtime 释放）；当前没有活跃事务时登记到延迟队列，
    /// 否则立即删除。`drop_ref` 幂等，重复释放同一 id 是空操作。
    pub fn release_extra_reference(&self, reference: u64) {
        if reference == 0 {
            return;
        }
        if self.engine.upgrade().is_none() {
            // 引擎已销毁：引用表随 runtime 一起消失。
            return;
        }
        if !has_active_ctx() {
            // JS 终结器 / 渲染循环等无事务路径：不能开新事务，登记到下一个事务里释放。
            schedule_ref_drop(reference);
            return;
        }
        if let Some(engine) = self.engine.upgrade() {
            with_engine_js(&engine, |ctx| drop_ref(&ctx, reference));
        }
    }

    /// 显式 `dispose()`：只由所有者调用（`ui.custom` 关闭对话框、widget/footer/header
    /// 被替换时）；同时释放引用表项。重复调用无副作用。
    ///
    /// 若当前线程没有活跃事务（宿主循环、JS 终结器），调用只把 id 放进延迟队列，真正的
    /// `dispose()` 执行推迟到下一次 `ActiveCtxGuard::enter`；重复调用仍只产生一次清理，
    /// 因为 `reference` 在入队前就已清零。
    pub fn dispose(&mut self) {
        let reference = self.reference;
        self.reference = 0;
        if reference == 0 {
            self.release_extra_refs();
            return;
        }
        if !has_active_ctx() {
            // JS 类终结器 / 引擎销毁路径：不能开新事务，登记到下一个事务里清理。
            if self.engine.upgrade().is_some() {
                schedule_disposal(reference);
            }
            self.release_extra_refs();
            return;
        }
        if let Some(engine) = self.engine.upgrade() {
            with_engine_js(&engine, |ctx| {
                if let Ok(value) = load_ref(&ctx, reference)
                    && let Some(object) = value.as_object()
                {
                    let _ = call_method::<Value>(&ctx, object, "dispose", &[]);
                }
                drop_ref(&ctx, reference);
            });
        }
        self.release_extra_refs();
    }

    /// 对组件对象执行一次调用；引擎已销毁时返回错误。
    ///
    /// 失败分两类：
    /// - 引擎已销毁：返回固定文案，不触碰 JS；
    /// - 引用缺失/值不是对象/`f` 抛错：在第二个 [`with_engine_js`] 里用 `take_exception`
    ///   取走**并清除**挂起的 JS 异常，返回其 `message`。若错误来自 Rust 侧（例如
    ///   `__kiana_refs` 未安装）而没有挂起异常，`take_exception` 会把 `undefined` 物化成
    ///   文本 `"undefined"`（`engine/promise.rs` 的 `fallback_error_from_value`）。
    ///
    /// 清除挂起异常是必要的：残留异常会被后续模块加载与 `execute_pending_job` 误读
    /// （见 `JsEngine::eval_script` 的说明）。
    fn with_object<R>(
        &self,
        f: impl for<'js> FnOnce(&Ctx<'js>, Object<'js>) -> rquickjs::Result<R>,
    ) -> Result<R, String> {
        let Some(engine) = self.engine.upgrade() else {
            return Err("extension UI is not available after the JS engine shut down".to_owned());
        };
        let reference = self.reference;
        let outcome: rquickjs::Result<R> = with_engine_js(&engine, |ctx| {
            let value = load_ref(&ctx, reference)?;
            let Some(object) = value.as_object() else {
                return Err(Exception::throw_message(&ctx, "component is not an object"));
            };
            f(&ctx, object.clone())
        });
        match outcome {
            Ok(value) => Ok(value),
            Err(_) => {
                let error = with_engine_js(&engine, |ctx| take_exception(&ctx, JsErrorKind::Host));
                Err(error.message)
            }
        }
    }

    /// 上报一次错误（同一组件同一错误只上报一次）。
    ///
    /// 去重键是完整消息文本，因此文本相同的两次失败只记一条。上报通道有两条，都会走：
    /// 若全局 `__kiana.recordError` 可用，调用 `__kiana.recordError("ui", message)`——
    /// 它进入引擎的错误队列（`JsEngine::take_errors`，`tests/js_component.rs` 据此断言
    /// 只上报一次）；`"ui"` 不在协议已知 kind 里，按 `Host` 归类。无论 JS 侧是否可用，
    /// 都再写一条 `tracing::warn!`（target `kiana_extensions::ui`）。
    fn report(&mut self, message: &str) {
        if !self.reported.insert(message.to_owned()) {
            return;
        }
        if let Some(engine) = self.engine.upgrade() {
            let text = message.to_owned();
            with_engine_js(&engine, |ctx| {
                if let Ok(kiana) = ctx.globals().get::<_, Object>("__kiana")
                    && let Ok(record) = kiana.get::<_, Function>("recordError")
                {
                    let value = rquickjs::String::from_str(ctx.clone(), &text)
                        .map(|value| value.into_value())
                        .unwrap_or_else(|_| Value::new_undefined(ctx.clone()));
                    let _ = record.call::<_, ()>((This(kiana), "ui", value));
                }
            });
        }
        tracing::warn!(target: "kiana_extensions::ui", "{message}");
    }
}

impl Drop for JsComponent {
    fn drop(&mut self) {
        // TS 里普通容器成员从不会被 owner `dispose()`；这里只释放引用（`Box.clear`、
        // overlay `hide`、GC 都走这条路径）。所有者语义见 [`OwnedJsComponent`]。
        self.release_reference();
    }
}

impl Component for JsComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        // JS 侧只认 Number：宽度先转成 f64；`this` 必须是组件对象本身，JS 子类里的
        // `super.render(...)` 才能读到同一实例的状态（设计文档 §5.2）。
        let width = width as f64;
        let outcome = self.with_object(|ctx, object| {
            let mut args = Args::new(ctx.clone(), 1);
            args.this(object.clone().into_value())?;
            args.push_arg(width)?;
            let value: Value = {
                let member: Value = object.get("render")?;
                let Some(function) = member.as_function() else {
                    return Err(Exception::throw_message(
                        ctx,
                        "component.render is not a function",
                    ));
                };
                args.apply(function)?
            };
            // `undefined`/`null` 是合法返回值：本次渲染为空，而不是错误。它走成功分支，
            // 因此会连同 `last_lines` 一起被清空（下一帧不会再显示旧内容）。
            if value.is_undefined() || value.is_null() {
                return Ok(Vec::new());
            }
            let Some(array) = value.as_array() else {
                return Err(Exception::throw_message(
                    ctx,
                    "component.render() did not return an array",
                ));
            };
            strings_from_array(array)
        });
        match outcome {
            // 只有成功才更新缓存；失败保留上一次成功的行，让终端继续显示上一帧。
            Ok(lines) => {
                self.last_lines = lines.clone();
                lines
            }
            Err(message) => {
                self.report(&message);
                self.last_lines.clone()
            }
        }
    }

    fn handle_input(&mut self, data: &str) {
        let data = data.to_owned();
        let outcome = self.with_object(|ctx, object| {
            match call_method::<Value>(ctx, &object, "handleInput", &[JsArg::String(data.clone())])?
            {
                Some(_) => Ok(()),
                // TS 的 `handleInput` 是可选成员：缺失时静默跳过。
                None => Ok(()),
            }
        });
        if let Err(message) = outcome {
            self.report(&message);
        }
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        // 事件按 `TuiMouseEvent` 的 serde 表示（camelCase/可选字段省略）序列化，
        // `tests/js_component.rs` 的 `handle_mouse_event_payload_uses_ts_field_names`
        // 锁定了字段名。序列化失败时静默返回 `None`：不进入 JS，也不上报。
        let event_json = serde_json::to_value(event).ok()?;
        let outcome = self.with_object(|ctx, object| {
            let args = [JsArg::Json(event_json.clone())];
            // `handleMouse` 是可选成员：缺失或不是函数时 `call_method` 返回 `None`，
            // 按“未处理”返回，不当作错误。
            let value = match call_method::<Value>(ctx, &object, "handleMouse", &args)? {
                Some(value) => value,
                None => return Ok(None),
            };
            mouse_result_from_js(value)
        });
        match outcome {
            Ok(result) => result,
            Err(message) => {
                self.report(&message);
                None
            }
        }
    }

    fn wants_key_release(&self) -> bool {
        // TS 的 `wantsKeyRelease` 是布尔属性（不是方法，`tests/js_component.rs` 用
        // `wantsKeyRelease: true` 构造）。这里只认真正的 JS 布尔；`undefined`、数字、
        // 对象等一律折算成 false，比 JS truthiness 更严格。
        self.with_object(|_ctx, object| {
            let value: Value = object.get("wantsKeyRelease")?;
            Ok(value.as_bool().unwrap_or(false))
        })
        .unwrap_or(false)
    }

    fn handles_input(&self) -> bool {
        // TS 的 `"handleInput" in component`。
        //
        // Rust 侧还要求属性是函数，因此比 `in` 更严格：存在但不是函数的 `handleInput`
        // 会返回 false（这样的组件在 TS 里会被认为有处理器，但调用时会失败）。
        self.with_object(|_ctx, object| {
            let value: Value = object.get("handleInput")?;
            Ok(value.as_function().is_some())
        })
        .unwrap_or(false)
    }

    fn invalidate(&mut self) {
        let outcome = self.with_object(|ctx, object| {
            call_method::<Value>(ctx, &object, "invalidate", &[]).map(|_| ())
        });
        if let Err(message) = outcome {
            self.report(&message);
        }
    }

    /// 容器按身份移除子项时需要向下转型（`Box::removeChild` / `TUIProxy.removeChild`）。
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    /// TS `isFocusable(component)`：只要 JS 对象有 `focused` 属性（含原型链，
    /// `JS_HasProperty`）就是可聚焦组件。TUI 通过本方法把 `focused` 写到 JS 对象上
    /// （`tui.ts:672-680`），扩展的 `StreamingInputPanel` 等组件因此能看到焦点状态。
    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        // `Object::contains_key` 走 `JS_HasProperty`（沿原型链查找）；属性值是什么类型
        // 都无所谓，`focused()` 读取时才按布尔解释。取不到对象（引擎已销毁或引用丢失）
        // 时这里的 `unwrap_or(false)` 让组件不可聚焦。
        let focusable = self
            .with_object(|_ctx, object| object.contains_key("focused"))
            .unwrap_or(false);
        if focusable { Some(self) } else { None }
    }
}

impl Focusable for JsComponent {
    fn focused(&self) -> bool {
        // 只接受 JS 布尔（`as_bool` 对其它类型返回 None，这里折算成 false）；
        // 读取失败（引擎销毁/引用丢失）同样返回 false，不经过 `report`。
        self.with_object(|_ctx, object| {
            let value: Value = object.get("focused")?;
            Ok(value.as_bool().unwrap_or(false))
        })
        .unwrap_or(false)
    }

    fn set_focused(&mut self, focused: bool) {
        let outcome = self.with_object(|_ctx, object| object.set("focused", focused));
        if let Err(message) = outcome {
            self.report(&message);
        }
    }
}

// ===========================================================================
// OwnedJsComponent
// ===========================================================================

/// 由**宿主所有者**持有的 JS 组件（`ui.setWidget` 工厂、footer/header 工厂、`ui.custom`
/// 对话框）。
///
/// TS 里这些组件的生命周期由所有者显式管理：widget/footer/header 被替换或清除时
/// `existing?.dispose()`（`interactive-mode.ts:2402-2458、2530、2559`），`custom` 关闭时
/// `component?.dispose?.()`（`interactive-mode.ts:2953-2967`）。Rust 宿主在替换/关闭时
/// 直接 Drop 组件，因此这里把 Drop 作为 `dispose()` 的触发点；普通容器成员仍用
/// [`JsComponent`]，Drop 只释放引用。
///
/// 注意：本类型**没有**覆写 [`Component::dispose`]，trait 方法用的是默认空实现；宿主通过
/// `Box<dyn Component>` 调用的 `dispose()` 不会立刻到达 JS（`kiana-interactive` 的
/// `OwnedComponent` 会转发这个 trait 方法，见 `kiana-interactive` 的 `extension_ui/widgets.rs`
/// 中 `OwnedComponent` 的 `dispose` 转发；行号在注释改造中会持续漂移，故只指符号）。
/// 实际的 JS `dispose()` 触发点是下面的 `Drop`：组件被替换/清除后由容器丢弃时执行。
pub struct OwnedJsComponent {
    inner: JsComponent,
}

impl OwnedJsComponent {
    /// 包装一个 JS 组件。
    pub fn new(inner: JsComponent) -> Self {
        OwnedJsComponent { inner }
    }

    /// 从 JS 对象构造（先存入 `__kiana_refs`）。
    pub fn from_object<'js>(engine: &Rc<JsEngine>, object: Object<'js>) -> rquickjs::Result<Self> {
        Ok(OwnedJsComponent {
            inner: JsComponent::from_object(engine, object)?,
        })
    }
}

impl Drop for OwnedJsComponent {
    fn drop(&mut self) {
        // 不区分当前是否有活跃 JS 事务：事务外会走延迟队列，在下一个
        // `ActiveCtxGuard::enter` 里补做。
        self.inner.dispose();
    }
}

impl Component for OwnedJsComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.inner.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.inner.handle_input(data);
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        self.inner.wants_key_release()
    }

    fn handles_input(&self) -> bool {
        self.inner.handles_input()
    }

    fn invalidate(&mut self) {
        self.inner.invalidate();
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        // 透明下钻：挂载树遍历（`collect_component_refs`）要能找到底层 JS 组件。
        self.inner.as_any()
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        self.inner.as_any_mut()
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        self.inner.as_focusable()
    }
}

// ===========================================================================
// JsEditorComponent
// ===========================================================================

/// 把 JS 编辑器对象实现为 [`EditorComponent`]（`ctx.ui.setEditorComponent` 的返回值）。
///
/// 与 [`JsComponent`] 共用引用表与错误处理；额外实现 `getText`/`setText` 与
/// `onSubmit`/`onChange`/`borderColor` 的赋值语义（TS 里是公开可写属性）。
pub struct JsEditorComponent {
    inner: JsComponent,
    /// `onSubmit`/`onChange`/`borderColor` 属性上当前回调的引用 id（替换时释放旧的，
    /// 避免每次宿主重新接线都往 `__kiana_refs` 里堆一个回不去掉的函数）。
    property_refs: RefCell<HashMap<String, u64>>,
}

impl JsEditorComponent {
    /// 从引用 id 构造。
    pub fn from_ref(engine: &Rc<JsEngine>, reference: u64) -> Self {
        JsEditorComponent {
            inner: JsComponent::from_ref(engine, reference),
            property_refs: RefCell::new(HashMap::new()),
        }
    }

    /// 从 JS 对象构造。
    pub fn from_object<'js>(engine: &Rc<JsEngine>, object: Object<'js>) -> rquickjs::Result<Self> {
        Ok(JsEditorComponent {
            inner: JsComponent::from_object(engine, object)?,
            property_refs: RefCell::new(HashMap::new()),
        })
    }

    /// 底层引用组件。
    pub fn component(&self) -> &JsComponent {
        &self.inner
    }

    /// 设置 JS 属性（`undefined` 表示清除回调）。
    ///
    /// 传 `JsArg::Undefined` 时是把属性值写成 `undefined`（不是 `delete`）；写入成功后
    /// 还会释放 `property_refs` 里该名字记录的旧回调引用。写入失败只上报，属性保持原值。
    fn set_property(&mut self, name: &str, value: JsArg) {
        let outcome = self.inner.with_object(|ctx, object| {
            let value = value.clone().into_js(ctx)?;
            object.set(name, value)?;
            Ok(())
        });
        if let Err(message) = outcome {
            self.inner.report(&message);
        } else if matches!(value, JsArg::Undefined) {
            // 清空回调：释放之前保存的引用。
            let previous = self.property_refs.borrow_mut().remove(name);
            if let Some(previous) = previous {
                self.inner.release_extra_reference(previous);
            }
        }
    }

    /// 替换属性上的回调引用：写入新值成功后释放旧引用（TS 依赖 GC 回收旧闭包）。
    ///
    /// 新旧引用相同时不释放：`property_refs` 仍用这个 id 记录当前属性值，此时若把它从
    /// 引用表删掉，映射里就会留下一个指向已删除表项的 id（JS 属性本身已持有函数值，
    /// 不会因此失效）。
    fn track_property_ref(&mut self, name: &str, reference: u64) {
        let previous = self
            .property_refs
            .borrow_mut()
            .insert(name.to_owned(), reference);
        if let Some(previous) = previous
            && previous != reference
        {
            self.inner.release_extra_reference(previous);
        }
    }
}

impl Drop for JsEditorComponent {
    fn drop(&mut self) {
        // 自定义 Drop 先于字段析构执行：这里释放全部属性回调引用；组件对象本身的
        // `__kiana_refs` 表项随后由 `inner` 字段的 `JsComponent::drop` 释放。
        let references: Vec<u64> = self
            .property_refs
            .get_mut()
            .drain()
            .map(|(_, reference)| reference)
            .collect();
        for reference in references {
            self.inner.release_extra_reference(reference);
        }
    }
}

impl Component for JsEditorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.inner.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.inner.handle_input(data);
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        self.inner.wants_key_release()
    }

    fn handles_input(&self) -> bool {
        self.inner.handles_input()
    }

    fn invalidate(&mut self) {
        self.inner.invalidate();
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        // TS `isFocusable(component)` 只看 `"focused" in component`（`tui.ts:672-680`）。
        let focusable = self
            .inner
            .with_object(|_ctx, object| object.contains_key("focused"))
            .unwrap_or(false);
        if focusable { Some(self) } else { None }
    }
}

impl Focusable for JsEditorComponent {
    fn focused(&self) -> bool {
        self.inner
            .with_object(|_ctx, object| {
                let value: Value = object.get("focused")?;
                Ok(value.as_bool().unwrap_or(false))
            })
            .unwrap_or(false)
    }

    fn set_focused(&mut self, focused: bool) {
        self.set_property("focused", JsArg::Bool(focused));
    }
}

impl EditorComponent for JsEditorComponent {
    fn get_text(&self) -> String {
        // `getText` 缺失（`Ok(None)`）或调用/转换失败时返回空串：这是 `&self` 读取接口，
        // 不调用 `report`，失败对调用方不可见（与 `set_text` 不同）。
        self.inner
            .with_object(|ctx, object| {
                let value = match call_method::<Value>(ctx, &object, "getText", &[])? {
                    Some(value) => value,
                    None => return Ok(String::new()),
                };
                Ok(js_string(&value).unwrap_or_default())
            })
            .unwrap_or_default()
    }

    /// `editor.getExpandedText?.() ?? editor.getText()`（`interactive-mode.ts:2656`）。
    ///
    /// 方法缺失（`Ok(None)`）、调用抛错或结果转换失败（`Err`）都回退到 `get_text()`；
    /// 比 TS 的 `??` 多覆盖了“抛错”这一分支（TS 会向外抛）。
    ///
    /// 注意：方法存在但返回 `undefined`/`null` 时**不会**回退——`call_method` 给出
    /// `Some(undefined)`，`js_string` 把它转成字符串 `"undefined"` 返回；TS 的 `??`
    /// 在这个情况下会取 `getText()`。
    fn get_expanded_text(&self) -> String {
        let expanded = self.inner.with_object(|ctx, object| {
            call_method::<Value>(ctx, &object, "getExpandedText", &[])
                .map(|value| value.map(|value| js_string(&value).unwrap_or_default()))
        });
        match expanded {
            Ok(Some(text)) => text,
            _ => self.get_text(),
        }
    }

    fn add_to_history(&mut self, text: &str) {
        self.call_optional("addToHistory", &[JsArg::String(text.to_owned())]);
    }

    fn insert_text_at_cursor(&mut self, text: &str) {
        self.call_optional("insertTextAtCursor", &[JsArg::String(text.to_owned())]);
    }

    fn set_padding_x(&mut self, padding: usize) {
        self.call_optional("setPaddingX", &[JsArg::num(padding as f64)]);
    }

    fn set_autocomplete_max_visible(&mut self, max_visible: usize) {
        self.call_optional(
            "setAutocompleteMaxVisible",
            &[JsArg::num(max_visible as f64)],
        );
    }

    fn set_autocomplete_provider(&mut self, provider: crate::ui::AutocompleteProviderObject) {
        // provider 先物化成 JS 对象并临时存进 `__kiana_refs`，才能作为 `JsArg::Ref` 传给
        // JS；调用返回后立即删除表项。之后是否还能用，取决于 JS 编辑器有没有自己保存这个
        // 参数——没有保存的话，下次 GC 就会回收它。
        // 类型不受支持（`autocomplete_provider_value` 返回 Err）或存储失败时静默放弃：
        // 既不调用 `setAutocompleteProvider`，也不上报。
        let Some(engine) = self.inner.engine.upgrade() else {
            return;
        };
        let reference = with_engine_js(&engine, |ctx| -> Option<u64> {
            let value = crate::ui::autocomplete_provider_value(&ctx, provider).ok()?;
            store_ref(&ctx, value).ok()
        });
        let Some(reference) = reference else {
            return;
        };
        self.call_optional("setAutocompleteProvider", &[JsArg::Ref(reference)]);
        let _ = with_engine_js(&engine, |ctx| -> rquickjs::Result<()> {
            drop_ref(&ctx, reference);
            Ok(())
        });
    }

    fn set_text(&mut self, text: &str) {
        // 与 `get_text` 相反：写入失败会经 `report` 上报（`&mut self` 路径可以上报）。
        let text = text.to_owned();
        let outcome = self.inner.with_object(|ctx, object| {
            call_method::<Value>(ctx, &object, "setText", &[JsArg::String(text.clone())])
                .map(|_| ())
        });
        if let Err(message) = outcome {
            self.inner.report(&message);
        }
    }

    fn set_on_submit(&mut self, callback: Option<EditorTextCallback>) {
        match callback {
            Some(callback) => {
                let engine = match self.inner.engine.upgrade() {
                    Some(engine) => engine,
                    None => return,
                };
                // 回调以 Rust 闭包注册进 JS：`Function::new` 把捕获的 `Rc<RefCell<...>>`
                // 包成 JS 函数，存进引用表后再写到 `onSubmit` 属性上（TS 里是直接赋值）。
                // 调用宿主回调期间该 `RefCell` 处于可变借用；若同一闭包在此期间被再次
                // 调用，`borrow_mut` 会 panic。
                let callback = Rc::new(RefCell::new(callback));
                let outcome = with_engine_js(&engine, |ctx| -> rquickjs::Result<()> {
                    let function =
                        Function::new(ctx.clone(), move |text: String| -> rquickjs::Result<()> {
                            (callback.borrow_mut())(&text);
                            Ok(())
                        })?;
                    let reference = store_ref(&ctx, function.into_value())?;
                    // 仍在上面打开的 JS 事务里：`set_property_ref` → `with_object` →
                    // `with_engine_js` 会复用当前 `Ctx`，不会嵌套 `with_js`。
                    self.set_property_ref(reference);
                    Ok(())
                });
                if let Err(error) = outcome {
                    // `error` 只是 rquickjs 的错误枚举；上报文本从挂起的 JS 异常取，
                    // `take_exception` 同时清除它，避免污染后续事务。
                    let message = with_engine_js(&engine, |ctx| {
                        take_exception(&ctx, JsErrorKind::Host).message
                    });
                    let _ = error;
                    self.inner.report(&message);
                }
            }
            None => self.set_property("onSubmit", JsArg::Undefined),
        }
    }

    fn set_on_change(&mut self, callback: Option<EditorTextCallback>) {
        match callback {
            Some(callback) => {
                let engine = match self.inner.engine.upgrade() {
                    Some(engine) => engine,
                    None => return,
                };
                // 与 `set_on_submit` 相同：Rust 闭包 → JS 函数 → 引用表 → 属性。
                let callback = Rc::new(RefCell::new(callback));
                let outcome = with_engine_js(&engine, |ctx| -> rquickjs::Result<()> {
                    let function =
                        Function::new(ctx.clone(), move |text: String| -> rquickjs::Result<()> {
                            (callback.borrow_mut())(&text);
                            Ok(())
                        })?;
                    let reference = store_ref(&ctx, function.into_value())?;
                    self.replace_property_ref("onChange", reference);
                    Ok(())
                });
                if let Err(error) = outcome {
                    // 与 `set_on_submit` 相同：上报文本来自挂起的 JS 异常。
                    let message = with_engine_js(&engine, |ctx| {
                        take_exception(&ctx, JsErrorKind::Host).message
                    });
                    let _ = error;
                    self.inner.report(&message);
                }
            }
            None => self.set_property("onChange", JsArg::Undefined),
        }
    }

    fn set_border_color(&mut self, callback: Option<EditorBorderColor>) {
        match callback {
            Some(callback) => {
                let engine = match self.inner.engine.upgrade() {
                    Some(engine) => engine,
                    None => return,
                };
                // `borderColor` 是 `(text) => string` 的函数值：闭包直接把着色结果返回给
                // JS，由 JS 编辑器在渲染时调用（Rust 侧不读取该属性）。
                let outcome = with_engine_js(&engine, |ctx| -> rquickjs::Result<()> {
                    let function = Function::new(
                        ctx.clone(),
                        move |text: String| -> rquickjs::Result<String> { Ok(callback(&text)) },
                    )?;
                    let reference = store_ref(&ctx, function.into_value())?;
                    self.replace_property_ref("borderColor", reference);
                    Ok(())
                });
                if let Err(error) = outcome {
                    // 与 `set_on_submit` 相同：上报文本来自挂起的 JS 异常。
                    let message = with_engine_js(&engine, |ctx| {
                        take_exception(&ctx, JsErrorKind::Host).message
                    });
                    let _ = error;
                    self.inner.report(&message);
                }
            }
            None => self.set_property("borderColor", JsArg::Undefined),
        }
    }
}

impl JsEditorComponent {
    /// 把引用设为 `onSubmit`（内部辅助，供闭包外使用）。
    fn set_property_ref(&mut self, reference: u64) {
        self.replace_property_ref("onSubmit", reference);
    }

    /// 把引用设为指定属性。
    ///
    /// 先把引用值写入 JS 属性，成功后才记入 `property_refs`（并释放同名旧引用）；写入失败
    /// 则立即释放新引用并上报，属性保持原值。
    fn replace_property_ref(&mut self, name: &str, reference: u64) {
        let outcome = self.inner.with_object(|ctx, object| {
            let value = load_ref(ctx, reference)?;
            object.set(name, value)?;
            Ok(())
        });
        match outcome {
            Ok(()) => self.track_property_ref(name, reference),
            Err(message) => {
                self.inner.release_extra_reference(reference);
                self.inner.report(&message);
            }
        }
    }

    /// 调用 JS 编辑器上的可选方法（缺失时静默跳过，TS 的 `editor.foo?.(...)`）。
    ///
    /// “缺失”也覆盖属性存在但不是函数的情况（`call_method` 的 `Ok(None)`）；真正调用时
    /// 抛错才会上报。
    fn call_optional(&mut self, name: &str, args: &[JsArg]) {
        let outcome = self
            .inner
            .with_object(|ctx, object| call_method::<Value>(ctx, &object, name, args).map(|_| ()));
        if let Err(message) = outcome {
            self.inner.report(&message);
        }
    }
}

impl std::fmt::Debug for JsComponent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JsComponent")
            .field("reference", &self.reference)
            .field("last_lines", &self.last_lines.len())
            .finish()
    }
}

impl std::fmt::Debug for JsEditorComponent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JsEditorComponent")
            .field("component", &self.inner)
            .finish()
    }
}

/// 便捷：把 `JsError` 转成可上报的文本。
///
/// 只取 `message`（不含 `name`/`stack`）；错误在引擎侧已按
/// `error instanceof Error ? error.message : String(error)` 物化过。
pub fn js_error_message(error: &JsError) -> String {
    error.message.clone()
}
