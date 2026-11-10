//! `modes/interactive/components` 目录（48 个文件）。
//!
//! 归属划分（见 `ARCHITECTURE.md` §3.4）：
//! - 9 个“无业务依赖的渲染小件”在 `kiana-ui-kit`（`kiana-tools` 的渲染器也要用，必须位于更低的层）：
//!   `auth-url` `bordered-loader` `countdown-timer` `diff` `dynamic-border` `keybinding-hints`
//!   `kiana-logo` `themed-text` `visual-truncate`；本模块只做再导出。
//! - 其余组件（含 48 个文件里依赖 session/扩展/选择器的部分）在本模块实现。
//! - `easter-egg-3d.lazy.ts` 是 TS 的动态 import 包装，Rust 侧并入 `easter_egg_3d`（编译期固定，无 lazy）。
//!
//! [`index`] 是 `components/index.ts` 的 barrel 再导出面；本模块只声明子模块并把 9 个
//! kiana-ui-kit 模块挂到与 TS 同名的路径下，不重复导出符号。
//!
//! 维护提示：这里的 `pub mod` 清单与 [`index`] 的 `pub use` 清单是两套独立列表。新增组件
//! 需要同时在本文件声明模块、在 `index.rs` 再导出，否则 crate 根（只做
//! `pub use components::index::*`，见 `lib.rs`）看不到该符号。子模块的声明顺序不参与任何
//! 运行时或解析逻辑。

// --- kiana-ui-kit 再导出（TS 文件在 components/ 下，实现在 kiana-ui-kit）---
pub use kiana_ui_kit::{
    auth_url, bordered_loader, countdown_timer, diff, dynamic_border, keybinding_hints, kiana_logo,
    themed_text, visual_truncate,
};

// --- 子模块 ---
pub mod approval_mode_indicator;
pub mod armin;
pub mod assistant_message;
pub mod bash_execution;
pub mod branch_summary_message;
pub mod compaction_summary_message;
pub mod config_selector;
pub mod custom_editor;
pub mod custom_entry;
pub mod custom_message;
pub mod earendil_announcement;
pub mod easter_egg_3d;
pub mod extension_editor;
pub mod extension_input;
pub mod extension_selector;
pub mod first_time_setup;
pub mod footer;
pub mod index;
pub mod login_dialog;
pub mod markdown_transform;
pub mod mcp_dialog;
pub mod mermaid;
pub mod model_selector;
pub mod oauth_selector;
pub mod permissions_dialog;
pub mod radius_login_selector;
pub mod scoped_models_selector;
pub mod session_selector;
pub mod session_selector_search;
pub mod settings_selector;
pub mod settings_submenu;
pub mod show_images_selector;
pub mod skill_invocation_message;
pub mod status_indicator;
pub mod tasks_dialog;
pub mod theme_selector;
pub mod thinking_selector;
pub mod tool_execution;
pub mod tree_selector;
pub mod trust_selector;
pub mod user_message;
pub mod user_message_selector;
