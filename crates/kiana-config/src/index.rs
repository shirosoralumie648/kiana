//! kiana-config 的再导出面（crate 的公开 barrel）。
//!
//! TS 没有与 `core/index.ts` 对应的统一 barrel：这些符号分别从
//! `core/{settings-manager,model-runtime,model-resolver,auth-storage,cache-warmer,...}.ts`
//! 直接 import。按 `ARCHITECTURE.md` §4.10「跨 crate 接口一律 `pub` 且在 `lib.rs` 再导出」，
//! 本模块汇总 kiana-config 的公开面，由 `lib.rs` 以 `pub use index::*;` 再导出。
//!
//! **为什么按模块再导出而不是全部摊平**：本 crate 目前只对无同名冲突的叶子模块做
//! `pub use ...::*` 扁平再导出，`models::registry` 与 `models::runtime`（门面/实现关系）
//! 以及 `auth` / `settings` 等模块保留命名空间，其符号只经模块路径访问。摊平任何一个
//! 模块前，都要确认其导出名不与已有扁平符号重名（`ambiguous_glob_reexports` 只由
//! 同名模块级项触发，方法不参与 glob 重导出）。使用方按
//! `kiana_config::settings::SettingsManager`、`kiana_config::models::resolver::resolve_model_reference`
//! 这样带模块的路径访问。
//!
//! # 这个模块做什么
//! 1. 以命名空间形式再导出各子模块，使 `kiana_config::index::settings` 这样的路径可用；
//! 2. 对四个无同名冲突的叶子模块额外做 `pub use ...::*`，让符号可以直接写成
//!    `kiana_config::<符号>`。
//!
//! `lib.rs` 末尾的 `pub use index::*;` 把这些再导出提到 crate 根，于是 crate 根同时提供
//! 模块路径与扁平符号两套入口。仓库内现有调用方都用带模块的路径；扁平符号是保留的公开
//! 便利入口。
//!
//! # 维护规则
//! - 新增子模块：先在 `lib.rs` 声明 `pub mod`；若还要扁平符号，再在本文件加一行
//!   `pub use crate::<模块>::*;`，并确认导出的名字与已有扁平符号不重名。
//! - 已以命名空间暴露的模块（`auth`、`cache`、`keybindings`、`models`、`settings`、
//!   `trust`）目前没有扁平再导出，因此这些符号只经模块路径访问。要新增时先确认名字
//!   不与已有扁平符号重名。
//! - 本文件只做再导出、不含实现；改导出列表就是改 crate 的公开面。

pub use crate::auth;
pub use crate::auth_guidance;
pub use crate::cache;
pub use crate::client_identity;
pub use crate::defaults;
pub use crate::keybindings;
pub use crate::models;
pub use crate::settings;
pub use crate::trust;

// 分层配置：`settings::layers` 仍可按模块路径访问；同时在 crate 根暴露主要类型，
// 让调用方不必记住 `settings::layers::` 前缀。名字与现有扁平符号无冲突。
pub use crate::settings::layers::{
    ArrayMergeMode, ConfigLayer, ConfigLayerSource, ConfigLayers, LayerLoadError, LayerLoadMode,
    ManagedPolicy, ManagedPolicyConflict, McpServerDecision, McpServerRule, MergedConfig,
};

// 无同名冲突的叶子模块：同时提供扁平符号（`kiana_config::get_client_identity` 等）。
// `resolve_config_value` 例外：模块内有一个同名函数，模块本身由 `lib.rs` 的
// `pub mod resolve_config_value;` 提供，这里只再导出其符号（否则 `use` 的模块名与
// glob 引入的同名函数在 value 命名空间冲突）。
// 因此 `kiana_config::resolve_config_value::<符号>` 仍可经 `lib.rs` 的模块声明访问。
pub use crate::auth_guidance::*;
pub use crate::client_identity::*;
pub use crate::defaults::*;
pub use crate::resolve_config_value::*;
