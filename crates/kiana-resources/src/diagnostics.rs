//! 资源诊断类型 `ResourceCollision` / `ResourceDiagnostic` 的再导出面。
//!
//! TS 在 `core/resource-loader.ts` 中定义、被 `core/diagnostics.ts` 再导出；`kiana-extensions`
//! （L9，`runner.ts`）与 `kiana-interactive` 都要用，而 `kiana-resources` 是 L10，故定义放在
//! `kiana_core::diagnostics`，本模块只 re-export（`ARCHITECTURE.md` §3.5）。
//!
//! 额外再导出 [`DiagnosticKind`] / [`ResourceType`]：TS 里它们是字段上的字面量联合，
//! Rust 侧需要具名枚举才能构造诊断；定义同样在 `kiana_core`，不新增语义。
//!
//! # 阅读指引
//!
//! 本文件没有逻辑，只有 `pub use`；类型的字段、序列化形状与默认值全部在
//! `kiana_core::diagnostics` 定义。修改本文件只会改变本 crate 的公开面，不会改变行为；
//! 要理解诊断如何产生、`message` 文案从哪来，应看资源加载方（例如 `crate::loader` 与
//! `crate::skills`）如何构造这些值。

pub use kiana_core::diagnostics::{
    DiagnosticKind, ResourceCollision, ResourceDiagnostic, ResourceType,
};
