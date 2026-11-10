//! `models.generated.ts`（309 行，生成物）：`MODELS` / `IMAGE_MODELS` / `CLASSIFIER_MODELS`
//! 三个只读对象的聚合面。
//!
//! TS 侧它把 42 个分片的常量重新聚合；Rust 侧数据源与顺序都在
//! [`crate::providers::catalog`]，本模块只再导出，不重复定义。
//!
//! 每个分片还导出 `<UPPER>_MODELS` / `<UPPER>_IMAGE_MODELS` / `<UPPER>_CLASSIFIER_MODELS`
//! 三个常量；它们的运行时内容就是 [`crate::providers::catalog`] 里对应 provider 的那一层，
//! provider 工厂（`providers/<id>.rs`）按需用 `catalog::get_models("<id>")` 等函数读取，
//! 因此不逐个生成 126 个常量（§8 第 5 条：不要手写 42 份重复代码）。
//!
//! # 修改禁忌与数据流
//!
//! TS 的 `models.generated.ts` 是生成物；本文件是手写的**再导出层**，不是生成物本身。
//! 真正的生成链是：
//!
//! ```text
//! crates/kiana-ai/data/<provider>.json
//!   -> build.rs（校验 sha256/structureHash，按固定 provider 顺序）
//!   -> $OUT_DIR/model_catalog.json
//!   -> providers/catalog.rs 的 LazyLock（解析一次）
//!   -> 本模块 pub use
//! ```
//!
//! 要改模型数据请改 `data/` 下的 JSON（必要时再改 `build.rs`），不要在任何 `.rs` 文件里
//! 抄写一份条目；也不要在本文件给每个 `pub use` 补文档——文档在 `providers::catalog` 的
//! 定义处，重复一份会与数据源漂移。`MODELS` 等是 `LazyLock<&'static ModelTable>`，
//! `get_*` 函数只是同一张表的只读查询，不复制数据。
//!
//! 仓库内（`crates/*`）目前没有代码引用本模块；它存在是为了让 TS 的
//! `models.generated.ts` 导入面在 Rust 侧有一个对应入口（与 crate 根的 `index` barrel
//! 定位相同）。

pub use crate::providers::catalog::{
    CLASSIFIER_MODELS, IMAGE_MODELS, MODELS, get_all_models, get_classifier_model,
    get_classifier_models, get_image_model, get_image_models, get_model, get_models,
};
