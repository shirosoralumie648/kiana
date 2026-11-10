//! delta/draft.ts。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! ## TS 原文件
//!
//! `draft.ts` 只有一条纯类型：
//!
//! ```text
//! export type Draft<T, Depth extends readonly unknown[] = []> =
//!   Depth["length"] extends 8 ? T
//!   : T extends null | boolean | number | string ? T
//!   : T extends (...args: never[]) => unknown ? T
//!   : T extends object ? { -readonly [Key in keyof T]: Draft<T[Key], [...Depth, unknown]> }
//!   : T;
//! ```
//!
//! 它把对象的只读字段变成可写、最多下潜 8 层，函数与原始值原样保留，供 tracker 的
//! `Change.state` 暴露“看起来可写、实际写入覆盖层”的视图（spec/chord-rest.md §2.30）。
//! `Depth` 的默认值是空元组，每递归一层追加一个 `unknown`；`Depth["length"] extends 8`
//! 因此在第 8 层停止下潜。
//!
//! ## 为什么 Rust 侧是空的
//!
//! Rust 没有结构化映射类型，无法表达这个递归条件类型；`serde_json::Value` 也不需要
//! 在类型层面区分只读与可写。因此本文件没有可移植的条目，只为与 TS 文件一一对应而保留。
//! 将来实现 tracker 时，spec 建议提供显式的 Draft API（get/set/remove/keys/len/iter），
//! 而不是用 `Deref`/`Index` 去模拟 Proxy。
