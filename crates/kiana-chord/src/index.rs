//! `index.ts` 再导出面（TS 的 91 行导出清单，逐项等价物）。
//!
//! TS 的 `index.ts` 是纯 re-export，无逻辑。Rust 侧同样：本模块只 `pub use` 其它模块的项，不定义
//! 任何东西。下面的对应关系按 TS 的导出分组：
//!
//! | TS 导出 | Rust |
//! | --- | --- |
//! | `type Cell` | `crate::cell::Cell`（`cell` 模块尚未移植，见下文） |
//! | `type AcceptOptions, LinkOptions` | [`crate::connection`] |
//! | `type Context, ContextKey` | [`crate::context`] |
//! | contract 的 8 个值 + 13 个类型 | [`crate::contract`] |
//! | `type EventOf, Resource, ResourceDeclaration, Subscribable, ValueOf` | [`crate::declare`] |
//! | `type Draft` | [`crate::delta`] |
//! | `ChordError`, `ChordErrorCode`, `forbidden`, `invalid`, `isChordError` | [`crate::errors`] |
//! | host 的 3 个值 + 类型 | [`crate::host`] |
//! | `copyJson`, `isJsonValue`, 类型 | [`crate::json`] |
//! | link 的 4 个值 + 类型 | [`crate::link`] |
//! | object 的类型 | [`crate::object`] |
//! | `defineRegistry` + 类型 | [`crate::registry`] |
//! | `type ReloadOptions, ReloadReport` | [`crate::reload`] |
//! | `replicatedState` + 类型 | [`crate::state`] |
//! | `type Ctx, GetOptions, Readable, RefStatus, Shape, StateEvent` | [`crate::types`] |
//! | `NativeWatch`, `Watch` + 类型 | [`crate::watch`] |
//! | `acceptWebSockets` + 类型 | [`crate::websocket`] |
//! | `PROTOCOL_VERSION`, `type Frame` | [`crate::wire`] |
//!
//! ## 与 crate 根的关系
//!
//! `lib.rs` 没有 `pub use index::*`：这些名字实际以 `kiana_chord::index::…` 访问。TS 的
//! `index.ts` 是包入口；Rust 侧保留同名模块只是给导出清单一个逐项对照的位置，不改变各类型
//! 的真实定义模块，也不构成额外的预导入面。
//!
//! ## 现阶段（阶段 1，lead 单元）
//!
//! 已实现并能再导出的模块：`contract`/`errors`/`registry`/`types`/`declare`/`context`/`json`/
//! `wire`（见下面的 `pub use`）。`cell`/`cluster`/`link`/`loader`/`manager`/`node`/`remote`/
//! `state`/`watch`/`websocket` 在当前副本里仍只有模块注释、没有任何项；`connection`/`host`/
//! `object`/`reload`/`delta`/`output`/`sqlite`/`telemetry_sqlite` 属于别的单元，进度见各模块
//! 自己的 `//!`。**本文件在合并阶段由 lead 补齐这些行**；现在不写它会引用不存在的项、让整个
//! crate 编译失败（并行规则要求每个单元的副本都能 `cargo check`）。`schema` 虽已实现，但 TS 的
//! `index.ts` 不导出它，所以这里也没有对应行。
//!
//! 注意：`CHORD_ERROR_CODES` 数组**不**在 `index.ts` 再导出（只在 `errors.ts` 内部使用，
//! 已在 `errors.rs` 的 `ChordErrorCode::ALL` 里给出同序数组）；`MemberDescription` 同理
//! （`contract.ts` 导出，`index.ts` 不导出）；`isFrame`/`toWireError`/`fromWireError` 不在
//! `index.ts`（只在 `wire.ts`）。

// ─── contract：8 个值 ────────────────────────────────────────────────────────
pub use crate::contract::{
    // 值
    bind,
    coverage,
    host_family,
    host_named,
    local,
    method,
    opaque,
    remote,
};
// ─── contract：13 个类型 ─────────────────────────────────────────────────────
pub use crate::contract::{
    Accepts, Adapter, AnyContract, Description, LocalContract, Placement, RemoteContract,
};
// TS 的 `HostFamily`/`HostNamed` 是 `Placement` 的两个变体；`ImplementationOf`/`InputOf`/`Ref`
// 是泛型映射类型（Rust 无对应物，见 `contract` 与 `types` 的文档）。

// ─── errors ─────────────────────────────────────────────────────────────────
pub use crate::errors::{
    ChordError, ChordErrorCode, ChordErrorReason, forbidden, invalid, is_chord_error,
};

// ─── registry ───────────────────────────────────────────────────────────────
pub use crate::registry::{Registry, RegistryTable, RegistryView, define_registry};

// ─── types ──────────────────────────────────────────────────────────────────
// TS 的 `index.ts` 只再导出 `Ctx, GetOptions, Readable, RefStatus, Shape, StateEvent`
// （`Exposed`/`RefBase`/`StateSource` 是 `types.ts` 的内部类型，不在公共面）。
pub use crate::types::{Ctx, GetOptions, Readable, RefStatus, Shape, StateEvent};

// ─── declare ────────────────────────────────────────────────────────────────
pub use crate::declare::{Resource, ResourceDeclaration, Subscribable};

// ─── context ────────────────────────────────────────────────────────────────
pub use crate::context::{Context, ContextKey};

// ─── json（TS index.ts：CopyJsonOptions、copyJson、isJsonValue、JsonValue）──────────
// `JsonRepresentation` 是 TS 的联合类型（strict JSON 的可表示形状）；Rust 侧就是
// [`serde_json::Value`]，没有单独的类型别名可导出。
pub use crate::json::{CopyJsonOptions, JsonValue, copy_json, is_json_value};

// ─── wire ───────────────────────────────────────────────────────────────────
pub use crate::wire::{Frame, PROTOCOL_VERSION};

// ─── publish（TS 的 index.ts 不导出它；保留给 crate 内部使用）─────────────────
// `crate::publish::{enqueue, hold_publications}`
