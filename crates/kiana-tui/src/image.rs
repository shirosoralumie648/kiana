//! `native-platform.ts` / `native-module-path.ts` 相关符号的再导出面。
//!
//! `native-platform.ts` 与 `native-module-path.ts` 的实现落在 [`crate::native`]（`native/` 子模块，
//! 见 `ARCHITECTURE.md` §8 第 17 条）；本文件只保留 TS 里 `native-platform.ts` /
//! `native-module-path.ts` 的再导出面，避免出现第二份实现。
//!
//! `index.ts` 的 `native-platform:` 分组导出的是 `getNativeClipboard` / `NativeClipboard`。
//! Rust 侧照此 `pub use`，并额外转出 `crate::native` 的其余公开符号（见下节）。
//!
//! 平台后端已实现（`crate::native::{linux_x11,darwin,win32}`，按 feature/cfg 选择），
//! 语义见 `spec/tui-core.md` §2.16/§2.17。
//!
//! ## 为什么单独保留这个文件
//!
//! `index.ts` 的 native-platform 分组只导出 `getNativeClipboard` / `NativeClipboard`
//! （components/image 分组另由 `crate::index` 镜像）；本模块补充 `crate::native` 的其余公开符号
//! （`ClipboardValue`、`ClipboardError`、`SharedNativeClipboard`、`get_native_platform_helper`、
//! `get_native_module_candidates`），这些不在 `index.ts` 的导出面里
//! （`get_native_module_candidates` 仅为对齐 `native-module-path.test.ts` 的可观察契约而保留）。
//! 本模块是 `crate::native` 的再导出面，并非 `index.ts` 的镜像，也不覆盖 `crate::native`
//! 的全部符号。
//! 这里**没有**本地实现：所有符号都转发到 [`crate::native`]；再写一份实现会出现两份真值。
//!
//! ## 导出项与实际归属
//!
//! - `NativeClipboard` / `SharedNativeClipboard`：平台剪贴板 trait 与 `Arc` 句柄；
//! - `ClipboardValue` / `ClipboardError`：剪贴板结果的三态封装与错误类型（`undefined` /
//!   `null` / 值的语义见 [`crate::native`] 模块文档的表格）；
//! - `get_native_clipboard`：取当前平台剪贴板（Linux 在启用 `x11` 时每次调用都重新检查
//!   `DISPLAY`）；
//! - `get_native_platform_helper`：darwin/win32 的 helper（Linux 恒 `None`）；
//! - `get_native_module_candidates`：TS 原生模块搜索路径的等价物；Rust 的后端在编译期选定，
//!   不经过它，保留它只为对齐 `native-module-path.test.ts` 的可观察契约。
//!
//! ## 注意事项
//!
//! - 后端由 Cargo feature 与 `#[cfg(target_os)]` 共同选择：真正决定代码是否编译进来的是
//!   对应平台的 `x11` / `darwin` / `win32` feature（`native` 本身是空 feature，见
//!   `Cargo.toml` 与 [`crate::native`]）。未启用时 `get_native_clipboard()` /
//!   `get_native_platform_helper()` 返回 `None`，调用方要按“平台没有剪贴板后端”处理。
//! - 下游 crate（例如 `kiana-core` 的剪贴板层）直接使用 `kiana_tui::native::…`；本文件是
//!   兼容/镜像面，不是唯一入口。

pub use crate::native::{
    ClipboardError, ClipboardValue, NativeClipboard, SharedNativeClipboard, get_native_clipboard,
    get_native_module_candidates, get_native_platform_helper,
};
