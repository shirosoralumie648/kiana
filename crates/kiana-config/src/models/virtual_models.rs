//! 虚拟模型定义（`kiana-session-core::virtual_models` 的类型在此接入）。
//!
//! TS 源：`packages/coding-agent/src/core/virtual-models.ts`（238 行）。
//!
//! **唯一实现**在 `kiana-session-core::virtual_models`（`ARCHITECTURE.md` §5 把该文件划给
//! `kiana-session-core`；`kiana-config` 依赖它）。本文件只做再导出，不复制任何状态或逻辑：
//! `VIRTUAL_MODEL_API` / `VIRTUAL_MODEL_STATE_ENTRY`、`VirtualModelDefinition` / `ModelRoute`
//! 等类型、`create_virtual_model` / `with_virtual_models` / `get_branch_selection` /
//! `get_virtual_model_state` / `find_latest_response` 等函数都直接来自底层 crate。
//!
//! # 为什么这里只有再导出
//! `ARCHITECTURE.md` §5 把虚拟模型的实现划给 `kiana-session-core`，但公开导入路径仍需要
//! 从本 crate 的 `models` 命名空间可达。于是本文件保留一个纯再导出模块：既没有第二份状态
//! 或实现，也不破坏既有导入路径。
//!
//! # 约束
//! - 这里只允许 `pub use`，不得添加类型、常量、函数或状态；任何逻辑都应加在
//!   `kiana-session-core::virtual_models`，否则两个路径会分叉。
//! - 导出的名字与底层是**同一项**（不是包装 newtype）：`tests/virtual_models_adapter.rs`
//!   断言主要导出名的常量值、函数与类型与底层一致；它并未引用全部导出名（如
//!   `ModelRoute`、`VirtualModelAuth` 等），删除未被该测试引用的导出不会使其失败。
//! - 不在这里给单个符号写 `///`：文档跟随底层定义，重写会与实现逐渐分离。
//!
//! # 调用方
//! 生产代码通常直接从 `kiana_session_core::virtual_models` 导入（例如
//! `crate::models::runtime` 的虚拟模型注册与路由）；本模块主要供依赖 `kiana-config`
//! 的消费方与适配测试使用。

pub use kiana_session_core::virtual_models::{
    ModelRoute, ModelRouteReason, ModelRouteRequest, RouteFailure, RouteFuture, RouteModelRef,
    THINKING_LEVELS, VIRTUAL_MODEL_API, VIRTUAL_MODEL_STATE_ENTRY, VirtualModelAuth,
    VirtualModelDefinition, VirtualModelRouteError, VirtualModelStateData, create_virtual_model,
    find_latest_response, get_branch_selection, get_virtual_model_state, is_virtual_model,
    unrouted_stream, with_virtual_models,
};
