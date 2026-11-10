//! `extensions/codemode/`（5 个文件）。
//!
//! 本目录与 TS 的 `packages/coding-agent/src/extensions/codemode/` 对应：除 `*.lazy.ts`
//! （动态 import 包装，并入同名模块，见 crate 根文档与 `ARCHITECTURE.md` §5.2）外，一个
//! TS 文件对应一个同名子模块（6 个 TS 文件 → 5 个模块：`execute.lazy.ts` 并入
//! [`execute`]），便于逐文件比对移植结果。本模块自身不实现逻辑，只声明子模块；
//! 对外的公开入口由 crate 根（`kiana-builtin-extensions/src/lib.rs`）按需再导出。
//!
//! # 子模块分工
//!
//! - [`execute`]：运行脚本的沙箱路径（worker 准入、输出整形、`models.*` 调用）。
//! - [`index`]：把 `codemode` 注册为扩展的工厂，以及设置读取与 `appendEntry` 桥接。
//! - [`renderer`]：TUI 中工具调用与结果的渲染器。
//! - [`tool`]：工具定义、参数 schema、提示文本与 details 数据类型。
//! - [`worker`]：只有文档；说明 TS 的 worker 副作用导入在 Rust 侧没有对应的运行逻辑。
//!
//! `pub mod` 的书写顺序只是源码组织，不构成加载顺序或 API 契约。

// --- 子模块 ---
pub mod execute;
pub mod index;
pub mod renderer;
pub mod tool;
pub mod worker;
