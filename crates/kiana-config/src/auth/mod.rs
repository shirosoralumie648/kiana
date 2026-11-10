//! `core/{auth-storage,runtime-credentials,models-store}.ts` 的内部拆分。
//!
//! 四个子模块各负责一件事：
//!
//! - [`storage`]：`auth.json` 的加锁后端（文件/内存两种实现）与 `StorageDocument`——后者
//!   在 Rust 侧复刻 JS 对象/属性语义、键序与字节形状，供 auth 与 models-store 共用；
//! - [`lock`]：文件锁（`proper-lockfile` 的等价物：目录锁 + 心跳 + 陈旧判定），
//!   `settings` 与 `trust` 也直接复用它；
//! - [`models_store`]：`models-store.json` 的按 provider 目录存储，复用 `storage` 的加锁
//!   与 0600/0700 语义；
//! - [`runtime_credentials`]：在任意 `CredentialStore` 之上叠加进程内 API key 覆盖层，
//!   不落盘。
//!
//! # 在 crate 中的位置
//!
//! 本模块是 `kiana-config` 的公开模块：`crate::models::runtime` 在构造 `ModelRuntime` 时
//! 创建 `AuthStorage` / `FileModelsStore` / `RuntimeCredentials`，coding-agent 的 app 与
//! CLI 检查也会直接引用 `auth::storage` / `auth::models_store` 的类型。主要的读写由
//! kiana-ai 的模型刷新/发布与凭证解析流程通过 `ModelsStore` / `CredentialStore` trait 发起；
//! 此外 CLI auth check（`--credentials` 的 `credentials.read`）、MCP OAuth 的
//! `McpOAuthStore`（直接用 `FileAuthStorageBackend::with_lock` 读写 `mcp-auth.json`）与
//! `read_stored_credential`（一次性读文件）也会直接使用这里的 storage 后端/文件。
//!
//! # 修改时的影响面
//!
//! 加锁顺序、`StorageDocument` 的属性语义或序列化细节会同时影响 `auth.json` 与
//! `models-store.json` 的磁盘字节，以及与其它进程（Node 版）对这些文件的互斥；`lock` 的
//! 语义还被 settings/trust 复用，改动前先看各自模块文档。

pub mod lock;
pub mod models_store;
pub mod runtime_credentials;
pub mod storage;
