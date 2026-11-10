//! prelude.js（`include_str!`）的门面。
//!
//! 唯一权威在 [`crate::runtime::prelude_source`]；本模块只做 `pub use`，不复制任何内容
//! （「同一 TS 文件只允许一个 crate 拥有实现」/ plannner 规则：`src/prelude.rs` 是
//! `runtime::prelude_source` 的 facade，单一权威）。
//!
//! TS 侧 `prelude-source.ts` 同时导出 4 个上限常量与 `PRELUDE_SOURCE`；这里保持同一组名字。
//!
//! # 再导出的内容
//!
//! | 名字 | 作用 |
//! |---|---|
//! | `MAX_STORE_VALUE_CHARS` / `MAX_STORE_TOTAL_CHARS` / `MAX_OUTPUT_CHARS` / `MAX_OUTPUT_ITEMS` | prelude 使用的四个上限；`js/prelude.js` 里已替换成字面量（`tests/prelude.rs` 锁定每处出现两次） |
//! | `PRELUDE_SOURCE` | `include_str!("../../js/prelude.js")`；17233 个 UTF-16 码元、全 ASCII |
//! | `PRELUDE_SOURCE_LEN` / `PRELUDE_HASH` / `PRELUDE_SOURCE_SHA256` | 逐字校验值；`PRELUDE_HASH` 进入 VM 指纹（`runtime/vm.rs`） |
//! | `fnv1a` / `js_length` | 生成/校验上述值的辅助函数，按 **UTF-16 码元**计数与哈希 |
//!
//! `crate::index` 与 `crate::portable` 只再导出 4 个上限；`PRELUDE_SOURCE` 与校验值只在本
//! 模块和 `crate::runtime::prelude_source` 可见。`tests/prelude.rs` 从本模块导入全部
//! 10 个名字并逐字节校验。

pub use crate::runtime::prelude_source::{
    MAX_OUTPUT_CHARS, MAX_OUTPUT_ITEMS, MAX_STORE_TOTAL_CHARS, MAX_STORE_VALUE_CHARS, PRELUDE_HASH,
    PRELUDE_SOURCE, PRELUDE_SOURCE_LEN, PRELUDE_SOURCE_SHA256, fnv1a, js_length,
};
