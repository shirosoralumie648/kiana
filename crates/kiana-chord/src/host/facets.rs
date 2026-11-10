//! host/facets.ts。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! ## 计划职责（当前未实现）
//!
//! `spec/chord-core.md` §8.1 的**建议布局**把 facet 的运行期接口分到本文件：`FacetApi`、
//! `defineFacet`、`install`、`load`、`resolve`、`facetRunChanged`、`serveInto`、
//! `startHostObject`。
//!
//! 术语：facet（插件面）由 `setup(api)` 与可选的 `uses`（它消费的契约）定义，
//! `defineFacet(setup, { uses })` 返回该定义；带 `name` 的 `Facet` 是宿主安装时使用的形式
//! （§2.5.1）。宿主安装（install）facet 时调用 `setup` 并传入 `FacetApi`：`serve` 登记类型
//! 工厂，`root` 转到宿主的 root 调用（未显式给 principal 时用宿主自己的 principal），
//! `registry` 取注册表视图，`signal` 在该次 facet 运行被替换、移除、reload 中止或 host 关闭
//! 时 abort（§2.5.1）。`load` 先经 loader 把模块引用解析成 facet，再逐个安装，install 之间
//! 没有 await 点（§2.5.4）。
//!
//! ## 现状与约束
//!
//! 本文件目前只有本段文档：没有类型、函数或重导出，因此 `host::facets` 下没有任何可调用
//! 条目，也没有调用方或测试。实现时必须保持 spec 给出的签名与错误文本，且不得在本 crate
//! 其它文件里另写一份 facet 逻辑（见上面的 TODO）。
