//! `declare.ts`：工厂通过 `self` 除状态 cell 之外声明的东西——资源与监听器的形状。
//!
//! TS 的这个文件全是类型；Rust 侧对应 trait 与结构体。`ValueOf`/`EventOf` 是条件类型，Rust 没有
//! 等价物，用泛型参数 + 方法表达（见 [`Subscribable`] 与 [`ListenerDecl`] 的文档）。
//!
//! # 边界
//!
//! 除 `Debug` 与 `ListenerSource::is_subscribable` 这类辅助实现外，本模块只定义类型与 trait；
//! 打开资源、注册监听器、保存状态等运行时实现都属于 `object` 单元（TS 的 `object.ts`；Rust 的
//! `object` 模块尚未移植）。因此这里定义的是**跨单元契约**：`version`/`deps`/`exclusive` 的语义
//! 就是 object 单元必须遵守的规则，改这些方法或字段的语义会同时影响所有实现方和按 TS 移植的
//! 调用方。
//!
//! # 为什么用 trait + `BoxFuture`
//!
//! TS 的声明是纯结构类型；运行时由 `object.ts` 用 `resourceOf`（`instanceof ResourceImpl` 或
//! facade 的 `WeakMap`）识别资源，监听器源则要求 `subscribe` 是函数。Rust 没有
//! 结构化类型，所以用 trait 表达“具备哪些能力”；`open`/`close` 返回
//! `BoxFuture<'static, …>`，允许实现是同步计算，也允许真正异步（数据库、socket），同时
//! 不需要把 async 函数的返回类型写进 trait。`'static` 的代价是：future 内不能借用 `&self`；
//! `options`/`value` 按值传入，可直接移入 future，只有需要共享的 `self` 字段才要 clone 或用
//! `Arc` 持有。

use std::fmt::Debug;
use std::sync::Arc;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::errors::ChordError;
use crate::types::Ctx;
use kiana_telemetry::index::BoxFuture;

/// 可以订阅的事件源（TS: `Subscribable<E>`）。
pub trait Subscribable<E: Send + 'static>: Send + Sync {
    /// 订阅事件；返回取消函数。
    ///
    /// 取消函数是 `FnOnce`（类型上只能调用一次），调用后不再收到事件。回调是 `Fn` 而不是
    /// `FnMut`：实现若需要累加状态，得用 `Mutex`/原子量等内部可变性。trait 不承诺回调在哪个
    /// 线程执行，调用方不应假设与 `subscribe` 同一线程。
    fn subscribe(
        &self,
        listener: Box<dyn Fn(E) + Send + 'static>,
    ) -> Box<dyn FnOnce() + Send + 'static>;
}

/// `open` 的选项：资源打开期间有效的取消信号。
///
/// TS: `open(options: { readonly signal: AbortSignal }): T | Promise<T>`。
#[derive(Debug, Clone)]
pub struct ResourceOpenOptions {
    /// 打开期间有效的取消信号。
    ///
    /// 用 `CancellationToken`（ARCHITECTURE §4.2 的取消原语）而不是 TS 的 `AbortSignal`；
    /// 实现通过 `is_cancelled()`/`cancelled().await` 决定何时放弃打开操作。
    pub signal: CancellationToken,
}

/// 资源声明（TS: `ResourceDeclaration<T>`）：对象打开并稍后关闭的东西（数据库、watch、socket）。
///
/// `version`：提升它会在下一次 reload 替换资源（原生值没有 schema 能检测变化）。
/// `deps`：必须先 live/open 的引用或资源。变化会替换资源。
/// `exclusive`：独占资源在 gate 处替换：先关旧再开新。
pub trait ResourceDeclaration<T: Send + 'static>: Send + Sync {
    /// 见类型文档（TS 的 `readonly version: number`）。
    fn version(&self) -> i64;

    /// 依赖（TS 的 `readonly deps?: readonly unknown[]`）。缺省空。
    fn deps(&self) -> Vec<ResourceDependency> {
        Vec::new()
    }

    /// 是否独占（TS 的 `readonly exclusive?: boolean`）。缺省 `false`。
    fn exclusive(&self) -> bool {
        false
    }

    /// 打开资源（TS 的 `open(options)`）。
    ///
    /// 返回 boxed future；失败以 [`ChordError`] 返回。打开成功的值由 [`Resource::value`] 暴露，
    /// 并在关闭时交回 [`ResourceDeclaration::close`]。
    fn open(&self, options: ResourceOpenOptions) -> BoxFuture<'static, Result<T, ChordError>>;

    /// 关闭资源（TS 的 `close?(value)`）。缺省什么也不做。
    ///
    /// TS 允许 `close` 同步或异步；这里统一成 boxed future。实现返回的 `Err` 表示关闭失败，
    /// 内存中的资源值仍会被丢弃（`value` 按值传入）。
    fn close(&self, _value: T) -> BoxFuture<'static, Result<(), ChordError>> {
        Box::pin(async { Ok(()) })
    }
}

/// `ResourceDeclaration.deps` 的元素：引用或资源。
///
/// TS 里是 `readonly unknown[]`（调用方传引用或资源；object.ts 用 `RefCore`/`ResourceImpl` 的判别
/// 属性区分）。Rust 侧显式建模，避免运行期猜测。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResourceDependency {
    /// 一个引用的 key（`RefCore` 的身份）。
    Ref(String),
    /// 一个资源的 key（`ResourceImpl` 的身份）。
    Resource(String),
}

/// 资源（TS: `Resource<T>`）。
pub trait Resource<T: Send + 'static>: Send + Sync + Debug {
    /// 资源 key（`self.resource(name, …)` 的名字）。
    fn key(&self) -> &str;

    /// 资源值；未打开前返回 `not_ready` 错误（TS: "Throws not_ready until opened"）。
    fn value(&self) -> Result<T, ChordError>;
}

/// `self.on` 的 `select`（TS 的 `(value: never) => Subscribable<unknown>`）。
///
/// 输入是 [`ListenerSource`] 求值后的值，输出是要继续订阅的源；返回 `Arc` 以便调用方保存或共享。
pub type SelectFn = Arc<dyn Fn(Value) -> Arc<dyn Subscribable<Value>> + Send + Sync>;

/// `self.on` 的处理函数（TS 的 `(event, ctx) => void | Promise<void>`）。
///
/// 事件按值传入，可以直接 move 进 `'static` future；`ctx` 以借用传入，不能带进 future，
/// 因此 future 内跨 `await` 仍需使用的 ctx 数据要先复制（例如 `ctx.root_id().to_owned()`）。
/// 错误以 [`ChordError`] 返回。
pub type ListenerHandler =
    Arc<dyn Fn(Value, &dyn Ctx) -> BoxFuture<'static, Result<(), ChordError>> + Send + Sync>;

/// 一个监听器（TS: `ListenerDecl`），即 `self.on(name, source, …)` 记录的东西。
///
/// `source` 是资源的已打开值或源本身；`select` 可选地把值映射成 `Subscribable`；
/// `fn` 是每次事件的处理函数（TS: `(event, ctx) => void | Promise<void>`）。
pub struct ListenerDecl {
    /// `self.on` 的源（TS 的 `readonly source: unknown`）。
    pub source: ListenerSource,
    /// 可选的 `select`（TS 的 `readonly select: ((value: never) => Subscribable<unknown>) | undefined`）。
    pub select: Option<SelectFn>,
    /// 处理函数。
    pub handler: ListenerHandler,
}

impl Debug for ListenerDecl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListenerDecl")
            .field("source", &self.source)
            .field("select", &self.select.is_some())
            .finish_non_exhaustive()
    }
}

/// `ListenerDecl.source` 的形态。
///
/// TS 的 `ValueOf<S>`：`S extends Subscribable<unknown> ? S : S extends Resource<infer T> ? T : S`。
/// Rust 没有条件类型，所以这里显式列出运行时可能出现的三类源；`select` 收到的值与
/// `handler` 收到的事件都来自这里（见 [`ListenerDecl`] 的字段说明）。
///
/// 三个变体互斥：`Subscribable` 已可直接订阅，`Resource` 要在资源打开后取出值，`Value` 是
/// 直接给出的值（它本身没有 `subscribe`，通常交给 `select` 映射成可订阅源）。变体本身不做
/// 校验，错误（例如资源还没打开）由消费它的单元在求值时产生。
#[derive(Clone)]
pub enum ListenerSource {
    /// 一个可订阅源（cell、derivation、引用状态成员等）。
    Subscribable(Arc<dyn Subscribable<Value>>),
    /// 一个资源：监听器收到它打开后的值。
    Resource(String),
    /// 直接给出源值。
    Value(Value),
}

impl ListenerSource {
    /// 该源是否是可订阅源。
    pub fn is_subscribable(&self) -> bool {
        matches!(self, ListenerSource::Subscribable(_))
    }
}

impl Debug for ListenerSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ListenerSource::Subscribable(_) => f.write_str("Subscribable(<source>)"),
            ListenerSource::Resource(key) => f.debug_tuple("Resource").field(key).finish(),
            ListenerSource::Value(value) => f.debug_tuple("Value").field(value).finish(),
        }
    }
}

/// TS 的 `ValueOf<S>`（类型级；Rust 侧见 [`ListenerSource`]）。
///
/// 保留为 trait 别名，供 `object` 单元在文档里引用。本 crate 目前没有任何实现，也没有方法：
/// 它只承载“TS 的条件类型在这里叫什么”的对应关系，运行期不产生值。
pub trait ValueOf {
    /// `ValueOf<S>`。
    type Value;
}

/// TS 的 `EventOf<S>`（类型级）。
///
/// 与 [`ValueOf`] 一样只是文档锚点：TS 的 `EventOf` 在 `S extends Subscribable<infer E>` 时取
/// 事件类型、在 `S extends Resource<infer T>` 时递归到 `T`；Rust 无法表达这种条件归约，
/// 运行期由 [`ListenerSource`] 的三个变体区分。
pub trait EventOf {
    /// `EventOf<S>`。
    type Event;
}
