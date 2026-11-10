//! `core/{trust-manager,project-trust}.ts`。
//!
//! 项目信任（project trust）的两个子模块：
//!
//! - [`manager`]：`trust.json` 的持久化与查询（`ProjectTrustStore`）、沿目录树继承、
//!   `TRUST_REQUIRING_*` 资源探测、UI 候选选项（`get_project_trust_options`）。
//!   不依赖 UI 或扩展。
//! - [`project_trust`]：判定引擎 `resolve_project_trusted`，把 override、扩展事件、store
//!   决定、`defaultProjectTrust`、UI 选择按固定顺序组合起来；UI 与扩展都以 trait 注入，
//!   避免 kiana-config 反向依赖上层。
//!
//! 两侧的关系是单向的：`project_trust` 调用 `manager`，`manager` 不知道判定规则。本模块不做
//! 再导出，调用方按完整路径引用（如 `kiana_config::trust::manager::ProjectTrustStore`）。
//!
//! TS 源：`core/trust-manager.ts`、`core/project-trust.ts`。

pub mod manager;
pub mod project_trust;
