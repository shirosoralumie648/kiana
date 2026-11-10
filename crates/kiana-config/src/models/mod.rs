//! `core/{model-config,provider-composer,model-runtime,model-registry,model-resolver,remote-catalog-provider,models-store}.ts` 的拆分。
//!
//! # 这个文件为什么存在
//! TS 把模型相关的逻辑放在 `core/` 下的若干文件里；Rust 移植按职责拆成多个模块，本文件
//! 只是它们的模块索引：**没有逻辑、没有类型、没有状态**，新代码不应加在这里。拆分的依据
//! 是「一个方向一个文件」：目录格式解析、HTTP 协议、覆盖层、provider 合成、运行时编排、
//! 对外门面、模型引用解析各自独立，避免单个文件继续膨胀。
//!
//! # 子模块分工
//! - [`catalog_http`]：远端目录的 URL 拼接、请求发送与响应解析（协议层；不处理状态码分支、
//!   刷新间隔与持久化）。
//! - [`remote_catalog`]：把远端目录缓存叠加到内置 provider 上（TTL、etag、404/501、
//!   个性化目录）。
//! - [`virtual_models`]：再导出 `kiana-session-core::virtual_models`，本 crate 不重复实现。
//! - [`config_file`]：`models.json` 的读取、JSONC 解析与 schema 校验。
//! - [`composer`]：models.json → 扩展注册 → OAuth `modifyModels` → `modelOverrides` 的
//!   provider 合成。
//! - [`resolver`]：模型引用、scope、CLI 选择与恢复等解析逻辑。
//! - [`runtime`]：`ModelRuntime`，编排层；其余模块大多由它驱动。
//! - [`registry`]：`ModelRegistry` 同步门面，扩展 API 的入口，底层由 `runtime` 支撑。
//!
//! # 依赖方向与阅读顺序
//! 内部依赖是有向无环的：`registry` → `runtime` → `remote_catalog` → `catalog_http` →
//! `config_file`，`runtime` 同时依赖 `composer`/`config_file`；`resolver` 只单向依赖
//! `runtime`（为它实现解析 trait）；`virtual_models` 不依赖本模块的任何文件。
//!
//! 建议按「读配置（`config_file`）→ 取目录（`catalog_http`/`remote_catalog`）→ 合成
//! （`composer`）→ 编排（`runtime`）→ 对外（`registry`）」的顺序阅读；`resolver` 与
//! `virtual_models` 可以最后看。
//!
//! # 约束
//! 本模块不定义新的持久化格式，可观察行为以 `spec/ca-config-models.md` 为准；对外 API 的
//! 增删会同时影响 `kiana-extensions` 的 JS 桥与 `kiana-coding-agent` 的再导出面。

pub mod catalog_http;
pub mod composer;
pub mod config_file;
pub mod registry;
pub mod remote_catalog;
pub mod resolver;
pub mod runtime;
pub mod virtual_models;
