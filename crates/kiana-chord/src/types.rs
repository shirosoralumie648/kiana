//! `types.ts`：引用（reference）与调用上下文（ctx）的类型。
//!
//! TS 的这个文件只有类型声明（没有运行期代码），Rust 侧对应的是 trait 与数据结构：
//!
//! | TS | Rust |
//! | --- | --- |
//! | `Ctx<P>`（`signal`/`rootId`/`codeVersion`/`principal`/`get`） | [`Ctx`] trait |
//! | `GetOptions<P>` | [`GetOptions`] |
//! | `StateEvent<T>`（`snap`/`ops`/`reset`） | [`StateEvent`] |
//! | `Readable<T>`（`value`/`subscribe`/`events`） | [`Readable`] |
//! | `StateSource<T>`（= `Readable<T>`） | [`StateSource`]（[`Readable`] 的别名） |
//! | `Exposed<T>`（`[EXPOSED]` 品牌） | [`Exposed`] |
//! | `RefStatus` | [`RefStatus`] |
//! | `Shape` | [`Shape`] |
//! | `RefBase<C>` | [`RefBase`] trait |
//!
//! `Ctx.get` 在 TS 里返回 `Promise<Ref<C>>`；Rust 没有映射类型，引用是显式 handle：[`Ref`]
//! （`RefBase` + 按名字访问成员）。具体实现在 `object::refs`（本地引用）与 `remote`（远程镜像）。
//!
//! 语义要点（来自 TS 注释，Rust 实现必须保持）：
//! - `subscribe` **立即**用当前值回调一次，返回取消函数。
//! - `events` 立即发 `{type:"snap", value}`，之后是 `ops` 或 `reset`。
//! - `status` 的 `loading` 也出现在上游链路重连时（保留最后值）。
//!
//! # 设计原因与边界
//!
//! 本文件不包含对象运行时的具体实现；除 [`GetOptions`]/[`StateEvent`]/[`RefStatus`] 的辅助方法、
//! [`Exposed`] 的构造与 `Default`、以及 `impl Ctx for Box<dyn Ctx>` 的转发外，只有 trait 与数据
//! 形状。`Readable`/`RefBase`/`Ctx` 会以 trait object 使用
//! （`Box<dyn Readable<Value>>`、`&dyn Ctx`），因此方法都保持对象安全；[`Readable::value`]
//! 返回 `T` 的所有权值（trait 上有 `T: Clone` 约束），调用方拿到的是独立副本，不依赖源的内部
//! 存储。
//!
//! “立即回调”是 [`Readable::subscribe`]/[`Readable::events`] 的实现约定，不是类型能强制的：
//! TS 的 `Fanout.subscribe`（cell.ts）在返回前同步回调当前值，从 TS 移植的调用方因此不会再
//! 自己读一次。新的 Rust 实现必须保持同样的可观察顺序，否则调用方会漏掉当前值。
//!
//! [`Ref::state_member`] 用 `Option` 表达两种“没有可读成员”的情况：当前 variant 没有该成员，
//! 或本引用没有接收状态（`GetOptions::state = false`）。引用整体的就绪/失效由
//! [`RefBase::status`] 表达。

use std::fmt::Debug;

use kiana_telemetry::TelemetrySpan;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::contract::AnyContract;
use crate::delta::Op;
use crate::errors::ChordError;
use kiana_telemetry::index::BoxFuture;

/// `get` 的选项。`principal`：对象为谁创建（缺省用宿主的自身 principal，即
/// `createHost({ self })`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GetOptions {
    /// 对象为哪个 principal 创建。缺省是宿主的自身 principal。
    pub principal: Option<Value>,
    /// `false`：只要方法的引用（不接收状态，除非同一对象的另一个引用要它），对象 live 即就绪。
    ///
    /// 缺省（`None`）等价于 `true`，由 [`GetOptions::wants_state`] 统一取值。
    pub state: Option<bool>,
}

impl GetOptions {
    /// 空选项。
    pub fn new() -> Self {
        GetOptions::default()
    }

    /// 设置 principal。
    pub fn with_principal(mut self, principal: Value) -> Self {
        self.principal = Some(principal);
        self
    }

    /// 设置 `state`。
    pub fn with_state(mut self, state: bool) -> Self {
        self.state = Some(state);
        self
    }

    /// TS 的 `options?.principal`。
    pub fn principal(&self) -> Option<&Value> {
        self.principal.as_ref()
    }

    /// TS 的 `options?.state ?? true` 的取值点（`state: false` 才关闭状态）。
    pub fn wants_state(&self) -> bool {
        self.state.unwrap_or(true)
    }
}

/// 状态事件（TS: `StateEvent<T>`）。
///
/// - `snap`：完整快照（订阅时的第一条）。
/// - `ops`：不可变操作批次 + 结果值。
/// - `reset`：迁移或重新绑定后的重置。
#[derive(Debug, Clone, PartialEq)]
pub enum StateEvent<T> {
    /// `{ type: "snap", value }`。
    Snap {
        /// 快照值。
        value: T,
    },
    /// `{ type: "ops", ops, value }`。
    Ops {
        /// 已解码的 op 批次（不是 wire 形式）。
        ops: Vec<Op>,
        /// 应用后的值。
        value: T,
    },
    /// `{ type: "reset", value }`。
    Reset {
        /// 重置后的值。
        value: T,
    },
}

impl<T> StateEvent<T> {
    /// TS 的 `event.value`。
    pub fn value(&self) -> &T {
        match self {
            StateEvent::Snap { value }
            | StateEvent::Ops { value, .. }
            | StateEvent::Reset { value } => value,
        }
    }

    /// TS 的 `event.type`。
    pub fn kind(&self) -> StateEventKind {
        match self {
            StateEvent::Snap { .. } => StateEventKind::Snap,
            StateEvent::Ops { .. } => StateEventKind::Ops,
            StateEvent::Reset { .. } => StateEventKind::Reset,
        }
    }

    /// 取出值（丢弃 ops）。
    pub fn into_value(self) -> T {
        match self {
            StateEvent::Snap { value }
            | StateEvent::Ops { value, .. }
            | StateEvent::Reset { value } => value,
        }
    }
}

/// `StateEvent.type`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StateEventKind {
    /// `"snap"`。
    Snap,
    /// `"ops"`。
    Ops,
    /// `"reset"`。
    Reset,
}

/// 取消订阅/取消监听（TS 的 `() => void`）。
///
/// `FnOnce`：调用一次即消费。取消只在被调用时发生；trait 没有约定丢弃时的行为，不要依赖
/// “丢弃自动取消”——需要停止回调时必须显式调用它。
pub type Unsubscribe = Box<dyn FnOnce() + Send + 'static>;

/// `Readable.subscribe` 的监听器（`(value: T) => void`）。
///
/// 回调收到的是**借用**的当前值（`&T`），需要保留时要自己 clone；`FnMut` 允许实现携带可变
/// 状态（如去重标记）。
pub type ValueListener<T> = Box<dyn FnMut(&T) + Send + 'static>;

/// `Readable.events` 的监听器（`(event: StateEvent<T>) => void`）。
///
/// 事件按值传入；第一条固定是 [`StateEvent::Snap`]，之后是 `Ops` 或 `Reset`。
pub type EventListener<T> = Box<dyn FnMut(StateEvent<T>) + Send + 'static>;

/// 可读状态源（TS: `Readable<T>`）：cell、derivation，或引用的状态成员。值按约定不可变。
///
/// TS 里 `value` 是只读属性；Rust 用方法（名字保持 `value`）。
pub trait Readable<T: Clone + Send + 'static>: Send + Sync {
    /// 当前值（克隆；不暴露内部存储的引用）。
    fn value(&self) -> T;

    /// **立即**用当前值回调一次，之后每次新值回调；返回取消函数。
    ///
    /// 回调是同步的：TS 的 `Fanout.subscribe` 在 `subscribe` 返回前调用一次，因此调用方不需要
    /// 再读 [`Readable::value`] 去补当前值。
    fn subscribe(&self, listener: ValueListener<T>) -> Unsubscribe;

    /// delta 流：立即一个快照，之后是 ops，或迁移/重新绑定后的 reset。
    ///
    /// 与 [`Readable::subscribe`] 一样，快照在 `events` 返回前同步发出。
    fn events(&self, listener: EventListener<T>) -> Unsubscribe;
}

/// 实现可以暴露为状态成员的东西：cell、derivation，或 held 对象的状态成员
/// （对象只有在它暴露的成员都有值时才会 live）。
///
/// TS: `type StateSource<T> = Readable<T>`。
pub use Readable as StateSource;

/// 品牌标记：由资源支撑的本地成员（`self.expose(res.x)`）；引用在资源 live 后读取它的值。
///
/// TS: `declare const EXPOSED: unique symbol; interface Exposed<T> { readonly [EXPOSED]: T }`。
/// Rust 侧只保留品牌类型（资源值由 `object::resource` 在运行期解析）。
#[derive(Debug, Clone, Copy)]
pub struct Exposed<T> {
    _marker: std::marker::PhantomData<fn() -> T>,
}

impl<T> Exposed<T> {
    /// 品牌值（仅用于类型标记，不含数据）。
    pub fn new() -> Self {
        Exposed {
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T> Default for Exposed<T> {
    fn default() -> Self {
        Exposed::new()
    }
}

/// 引用状态（TS: `RefStatus`）。
///
/// `serde` 用 `lowercase` 序列化；[`RefStatus::as_str`] 给出的字符串与 TS 字面量一致，
/// `tests/types.rs` 的集成测试 `reference_types_compile_and_keep_their_shape` 断言四个值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefStatus {
    /// 尚未就绪（上游链路重连时也会回到它，保留最后值）。
    Loading,
    /// 就绪且可用。
    Live,
    /// 调用方已 `release()`。
    Released,
    /// 对象消失（`error` 说明原因）。
    Gone,
}

impl RefStatus {
    /// 与 TS 字符串字面量一致。
    pub fn as_str(self) -> &'static str {
        match self {
            RefStatus::Loading => "loading",
            RefStatus::Live => "live",
            RefStatus::Released => "released",
            RefStatus::Gone => "gone",
        }
    }
}

/// 引用的 shape 快照：provider 当前 variant 与成员，以及 provider 契约指纹。
///
/// 序列化时 `variant: None` 会被省略（字段上的 `skip_serializing_if`），`members`/`contract`
/// 总是出现；`tests/types.rs` 的 `reference_types_compile_and_keep_their_shape` 锁定这个
/// JSON 形状。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shape {
    /// 当前 variant 名；local 契约没有 variant（`None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// 成员的（插入序）名字。
    pub members: Vec<String>,
    /// provider 契约的指纹。
    pub contract: String,
}

/// `RefBase.onShape` 的监听器。
pub type ShapeListener = Box<dyn Fn(&Shape) + Send + 'static>;
/// `RefBase.onStatus` 的监听器（`status`, `error`）。
pub type StatusListener = Box<dyn Fn(RefStatus, Option<&ChordError>) + Send + 'static>;

/// 引用的共同部分（TS: `RefBase<C>`）。
///
/// 具体实现：`object::refs`（本地引用）与 `remote`（远程镜像）。`ready` 对应 TS 的
/// `Promise<void>`：就绪时 `Ok`，激活失败或 `gone` 时是错误（`aborted`/`unavailable` 等）。
pub trait RefBase: Send + Sync + Debug {
    /// 绑定这个引用时用的消费者契约 token（TS: `readonly contract: C`）。
    fn contract(&self) -> &AnyContract;

    /// 当前状态。
    fn status(&self) -> RefStatus;

    /// 就绪 future（TS: `readonly ready: Promise<void>`）。
    ///
    /// future 是 `'static`，不借用 `self`：实现必须自己持有（例如用 `Arc`）完成通知所需的
    /// 状态，不能把 `&self` 借进 future。
    fn ready(&self) -> BoxFuture<'static, Result<(), ChordError>>;

    /// 当前 variant（local 契约为 `None`）。
    fn variant(&self) -> Option<String>;

    /// 最近一次 shape（尚未收到时为 `None`）。
    fn shape(&self) -> Option<Shape>;

    /// 引用为何 gone（若 gone）。
    fn error(&self) -> Option<ChordError>;

    /// shape 监听器：注册后不再重复当前值（TS 的 `onShape` 没有"立即回调"约定）。
    fn on_shape(&self, listener: ShapeListener) -> Unsubscribe;

    /// 状态监听器：每次状态变化后调用（loading → live、重连时 live → loading、released、
    /// gone 带 error）。UI 靠 `gone` 重新 `get`。
    fn on_status(&self, listener: StatusListener) -> Unsubscribe;

    /// 释放引用。
    fn release(&self);
}

/// Rust 侧的引用 handle：`RefBase` + 按名字访问成员。
///
/// TS 的 `Ref<C>` 是映射类型（`RefBase<C> & { variant } & { [S in state]: Readable<…> } &
/// { [M in methods]: Call<…> }`）；Rust 没有映射类型，所以本地/远程引用都实现这个 trait，
/// 调用方按契约里的成员名访问（`state_member("text")`、`call("send", args, ctx)`）。
pub trait Ref: RefBase {
    /// 状态成员（只有当前 variant 的、且本引用接收状态的成员才有值）。
    fn state_member(&self, name: &str) -> Option<Box<dyn Readable<Value>>>;

    /// 调用方法。`args` 为 `None` 表示无参数（TS 的 `args === undefined`）。
    ///
    /// 返回的 future 借用 `self`/`name`/`ctx`（签名里的 `'a`）；三者只要活得比 future 长即可，
    /// future 内部跨 `await` 继续使用它们不需要调用方 clone。只有要把数据移入 `'static` 任务、
    /// 或让其活过 `'a` 时才需要 clone/own。
    fn call<'a>(
        &'a self,
        name: &'a str,
        args: Option<Value>,
        ctx: &'a dyn Ctx,
    ) -> BoxFuture<'a, Result<Option<Value>, ChordError>>;
}

/// 一次 root（一段短的 Chord 调用树）的显式调用上下文。
///
/// 用它发起的调用携带它的 root id 与 code version；收到这个 ctx 的调用结束时，它的 `get` 引用
/// 被释放。
///
/// TS: `interface Ctx<P> { signal; rootId; codeVersion; principal; get(...) }`。
pub trait Ctx: Send + Sync + Debug {
    /// 取消信号（TS: `readonly signal: AbortSignal`）。Rust 按 ARCHITECTURE §4.2 用
    /// `CancellationToken`。
    fn signal(&self) -> &CancellationToken;

    /// TS 的 `signal.reason`：仅当原因是一个错误时需要它（`ctx.check` 把 `superseded` 与
    /// `aborted` 分开）。缺省 `None`。
    fn abort_cause(&self) -> Option<ChordError> {
        None
    }

    /// 本次调用树的 id。
    fn root_id(&self) -> &str;

    /// 本次调用使用的 code version。
    fn code_version(&self) -> i64;

    /// root 以谁的身份行事：入口处是调用方，对象内部是宿主的自身 principal。
    fn principal(&self) -> &Value;

    /// 为本次调用取一个引用，调用结束时释放。
    ///
    /// 就绪时返回引用（已 live，且带它的 variant 的全部状态成员——除非 `state: false`）。
    /// 失败时：激活错误、`aborted`（ctx 被取消或调用先结束）、或宿主 `readyTimeout` 后的
    /// `unavailable/ready_timeout`；此时引用已被释放。
    ///
    /// 返回的 future 借用 `self` 与 `contract`（`'a`）。按 TS 的 `Ctx.get` 文档，取到的引用
    /// 在接收这个 ctx 的调用结束时统一释放，调用方不需要单独管理它们的生命周期。
    fn get<'a>(
        &'a self,
        contract: &'a AnyContract,
        input: Value,
        options: GetOptions,
    ) -> BoxFuture<'a, Result<Box<dyn Ref>, ChordError>>;

    /// TS 的 `ctx.telemetry`？TS 的 `Ctx` 没有 telemetry 成员；`Context`（`context/index.ts`）
    /// 才有。这里不提供，避免两个"ctx"混淆。
    fn telemetry(&self) -> Option<&dyn TelemetrySpan> {
        None
    }
}

/// 允许 `&dyn Ctx` 与 `Box<dyn Ctx>` 互换使用（`Ctx` 作为 trait object 传递）。
impl Ctx for Box<dyn Ctx> {
    fn signal(&self) -> &CancellationToken {
        (**self).signal()
    }

    fn abort_cause(&self) -> Option<ChordError> {
        (**self).abort_cause()
    }

    fn root_id(&self) -> &str {
        (**self).root_id()
    }

    fn code_version(&self) -> i64 {
        (**self).code_version()
    }

    fn principal(&self) -> &Value {
        (**self).principal()
    }

    fn get<'a>(
        &'a self,
        contract: &'a AnyContract,
        input: Value,
        options: GetOptions,
    ) -> BoxFuture<'a, Result<Box<dyn Ref>, ChordError>> {
        (**self).get(contract, input, options)
    }

    fn telemetry(&self) -> Option<&dyn TelemetrySpan> {
        (**self).telemetry()
    }
}
